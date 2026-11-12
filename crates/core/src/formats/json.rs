//! JSON lines: one object per line. Top-level keys become fields; nested objects are flattened
//! into dotted names (`http.status`) down to [`JSON_FLATTEN_DEPTH`]; arrays and deeper objects
//! are one field holding the raw JSON. `ts`/`time`/`@timestamp`… fill the time (RFC 3339 strings
//! or epoch numbers), `level`/`severity`… the level (words, or bunyan / pino numbers),
//! `msg`/`message` the message.
//!
//! No recursion past the flatten depth: deeper values are skipped by counting brackets, so a
//! line of a million `[` costs one pass and no stack.

use super::{FieldNames, FormatKind, Ids, JSON_FLATTEN_DEPTH, Parser, Value, at, cut, emit, quoted, reset, sp, unescape, whole};
use crate::{Parsed, Span};

/// One JSON object per line.
#[derive(Debug, Default)]
pub struct JsonParser;

impl Parser for JsonParser {
    fn kind(&self) -> FormatKind {
        FormatKind::JsonLines
    }

    fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed {
        let mut p = Parsed::default();
        self.parse_into(line, names, &mut p);
        p
    }

    fn parse_into(&self, line: &[u8], names: &FieldNames, out: &mut Parsed) {
        parse_line(line, names, out);
    }

    fn field_text(&self, line: &[u8], span: Span) -> String {
        let raw = span.slice(line).unwrap_or_default();
        let was_string = span.start > 0 && at(line, span.start as usize - 1) == b'"';
        if was_string { unescape(raw) } else { String::from_utf8_lossy(raw).into_owned() }
    }
}

struct Sink<'a, 'n> {
    line: &'a [u8],
    ids: Ids<'n>,
    out: &'a mut Parsed,
    has_msg: bool,
    /// The dotted prefix of the object being walked (`http.`); empty at the top.
    key: Vec<u8>,
}

/// Parse `line` into `out`; whether it was a JSON object.
pub(crate) fn parse_line(line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    let line = cut(line);
    reset(out);
    let start = ws(line, 0);
    let start = if line.get(start..start + 3) == Some(b"\xEF\xBB\xBF") { ws(line, start + 3) } else { start };
    if at(line, start) != b'{' {
        whole(out, line);
        return false;
    }
    let mut sink = Sink { line, ids: Ids::new(names), out, has_msg: false, key: Vec::new() };
    let ok = object(&mut sink, start, 0).is_some_and(|end| ws(line, end) >= line.len());
    let has_msg = sink.has_msg;
    if !ok {
        whole(out, line);
        return false;
    }
    if !has_msg {
        out.message = sp(0, line.len());
    }
    true
}

fn ws(line: &[u8], mut i: usize) -> usize {
    while matches!(at(line, i), b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Walk the object whose `{` is at `i`; the index after its `}`.
fn object(s: &mut Sink<'_, '_>, i: usize, depth: usize) -> Option<usize> {
    let line = s.line;
    let prefix = s.key.len();
    let mut i = ws(line, i + 1);
    if at(line, i) == b'}' {
        return Some(i + 1);
    }
    loop {
        if at(line, i) != b'"' {
            return None;
        }
        let (ks, ke, kesc, next) = quoted(line, i)?;
        i = ws(line, next);
        if at(line, i) != b':' {
            return None;
        }
        i = ws(line, i + 1);
        let raw_key = line.get(ks..ke)?;
        let c = at(line, i);
        if c == b'{' && depth + 1 < JSON_FLATTEN_DEPTH {
            set_key(&mut s.key, prefix, raw_key, kesc);
            s.key.push(b'.');
            i = object(s, i, depth + 1)?;
            s.key.truncate(prefix);
        } else {
            let (value, end) = match c {
                b'"' => {
                    let (vs, ve, _, n) = quoted(line, i)?;
                    (Value::Quoted(vs, ve), n)
                }
                b'{' | b'[' => {
                    let end = skip_container(line, i)?;
                    (Value::Raw(i, end), end)
                }
                _ => {
                    let mut j = i;
                    while j < line.len() && !matches!(at(line, j), b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                        j += 1;
                    }
                    if j == i {
                        return None;
                    }
                    (Value::Bare(i, j), j)
                }
            };
            if prefix == 0 && !kesc {
                emit(line, &mut s.ids, s.out, &mut s.has_msg, raw_key, value);
            } else {
                set_key(&mut s.key, prefix, raw_key, kesc);
                emit(line, &mut s.ids, s.out, &mut s.has_msg, &s.key, value);
                s.key.truncate(prefix);
            }
            i = end;
        }
        i = ws(line, i);
        match at(line, i) {
            b',' => i = ws(line, i + 1),
            b'}' => return Some(i + 1),
            _ => return None,
        }
    }
}

fn set_key(key: &mut Vec<u8>, prefix: usize, raw: &[u8], escaped: bool) {
    key.truncate(prefix);
    if escaped {
        key.extend_from_slice(unescape(raw).as_bytes());
    } else {
        key.extend_from_slice(raw);
    }
}

/// Skip the object or array starting at `i` (any depth, no recursion); the index after it.
fn skip_container(line: &[u8], i: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = i;
    while j < line.len() {
        match at(line, j) {
            b'"' => {
                j = quoted(line, j)?.3;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Timestamp};

    fn fields(names: &FieldNames, line: &[u8], p: &Parsed) -> Vec<(String, String)> {
        p.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), JsonParser.field_text(line, *s))).collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn fixture_line() {
        let n = FieldNames::default();
        let line = br#"{"ts":"2026-11-12T07:41:03.118Z","level":"warn","service":"api","msg":"slow request","request_id":"r-0000beef","status":200,"latency_ms":312}"#;
        let p = JsonParser.parse(line, &n);
        assert_eq!(p.time, Some(Timestamp(1_794_469_263_118_000)));
        assert_eq!(p.level, Some(Level::Warn));
        assert_eq!(p.message.slice(line), Some(&b"slow request"[..]));
        assert_eq!(fields(&n, line, &p), pairs(&[("service", "api"), ("request_id", "r-0000beef"), ("status", "200"), ("latency_ms", "312")]));
    }

    #[test]
    fn table() {
        let n = FieldNames::default();
        // (line, time rfc3339, level, message, fields)
        let cases: &[(&str, Option<&str>, Option<Level>, &str, &[(&str, &str)])] = &[
            // pino / bunyan: numeric level, epoch millis
            (r#"{"level":30,"time":1794469263118,"pid":7,"msg":"hi"}"#, Some("2026-11-12T07:41:03.118Z"), Some(Level::Info), "hi", &[("pid", "7")]),
            (r#"{"level":50,"time":1794469263,"msg":"x"}"#, Some("2026-11-12T07:41:03.000Z"), Some(Level::Error), "x", &[]),
            // nested objects flatten, arrays stay raw, escapes resolve
            (
                r#"{"@timestamp":"2026-11-12T07:41:03+01:00","log":{"level":"ERROR"},"http":{"status":503,"req":{"path":"/a\"b"}},"tags":["a","b"],"message":"line\nbreak"}"#,
                Some("2026-11-12T06:41:03.000Z"),
                Some(Level::Error),
                "line\nbreak",
                &[("http.status", "503"), ("http.req.path", "/a\"b"), ("tags", r#"["a","b"]"#)],
            ),
            // no message key: the whole line; a level that is not a level stays a field
            (
                r#"{"level":"chatty","a":true,"b":null}"#,
                None,
                None,
                r#"{"level":"chatty","a":true,"b":null}"#,
                &[("level", "chatty"), ("a", "true"), ("b", "null")],
            ),
            // a time that does not parse stays a field
            (r#"{"time":"soon","msg":"m"}"#, None, None, "m", &[("time", "soon")]),
            // whitespace and an empty object
            ("  { \"msg\" : \"spaced\" , \"o\" : { } }  ", None, None, "spaced", &[]),
            // escaped keys
            (r#"{"k\u00e9y":"v"}"#, None, None, r#"{"k\u00e9y":"v"}"#, &[("kéy", "v")]),
        ];
        for (line, time, level, msg, want) in cases {
            let b = line.as_bytes();
            let p = JsonParser.parse(b, &n);
            assert_eq!(p.time.map(Timestamp::to_rfc3339).as_deref(), *time, "{line}");
            assert_eq!(p.level, *level, "{line}");
            assert_eq!(JsonParser.field_text(b, p.message), *msg, "{line}");
            assert_eq!(fields(&n, b, &p), pairs(want), "{line}");
        }
    }

    #[test]
    fn not_json_is_whole_line() {
        let n = FieldNames::default();
        for line in [
            "",
            "not json",
            "{",
            r#"{"a":1"#,
            r#"{"a":}"#,
            r#"{"a" 1}"#,
            r#"{"a":1} trailing"#,
            r#"{a:1}"#,
            r#"{"a":"unterminated}"#,
            "[1,2]",
            "{\"a\":[1,{]}",
        ] {
            let p = JsonParser.parse(line.as_bytes(), &n);
            assert_eq!(p, Parsed { message: sp(0, line.len()), ..Parsed::default() }, "{line}");
            assert!(!parse_line(line.as_bytes(), &n, &mut Parsed::default()));
        }
    }

    #[test]
    fn deep_nesting_does_not_recurse() {
        let n = FieldNames::default();
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(200_000), "]".repeat(200_000));
        let p = JsonParser.parse(deep.as_bytes(), &n);
        // Cut to MAX_LINE_BYTES, so the array never closes: the line does not fit.
        assert_eq!(p.fields.len(), 0);
        let objs = format!("{}1{}", "{\"a\":".repeat(10_000), "}".repeat(10_000));
        let p = JsonParser.parse(objs.as_bytes(), &n);
        assert_eq!(p.fields.len(), 1);
        assert_eq!(n.name(p.fields[0].0).as_deref(), Some("a.a.a.a"));
    }
}
