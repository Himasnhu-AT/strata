//! logfmt: `key=value key2="quoted value"`. `time`/`ts`… fill the time, `level`/`lvl`… the level,
//! `msg`/`message` the message (the whole line when there is none); every other pair is a field.
//! Bare words between pairs are skipped.

use super::{FieldNames, FormatKind, Ids, Parser, Value, at, cut, emit, quoted, reset, sp, unescape, whole};
use crate::{Parsed, Span};

/// `key=value` pairs.
#[derive(Debug, Default)]
pub struct LogfmtParser;

impl Parser for LogfmtParser {
    fn kind(&self) -> FormatKind {
        FormatKind::Logfmt
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
        let was_quoted = span.start > 0 && at(line, span.start as usize - 1) == b'"';
        if was_quoted { unescape(raw) } else { String::from_utf8_lossy(raw).into_owned() }
    }
}

fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r')
}

/// Parse `line` into `out`; whether it looks like logfmt (it starts with a pair, has at least
/// two, and pairs are at least as many as bare words).
pub(crate) fn parse_line(line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    let line = cut(line);
    reset(out);
    let mut ids = Ids::new(names);
    let (mut pairs, mut bare, mut first_is_pair, mut has_msg) = (0usize, 0usize, None::<bool>, false);
    let mut i = 0;
    let n = line.len();
    while i < n {
        while i < n && space(at(line, i)) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let ks = i;
        while i < n && !space(at(line, i)) && !matches!(at(line, i), b'=' | b'"') {
            i += 1;
        }
        let ke = i;
        if at(line, i) != b'=' || ke == ks {
            // A bare word or stray text: skip it (a quoted chunk as one).
            first_is_pair.get_or_insert(false);
            bare += 1;
            if at(line, i) == b'"' {
                i = quoted(line, i).map_or(n, |q| q.3);
            }
            while i < n && !space(at(line, i)) {
                i += 1;
            }
            continue;
        }
        first_is_pair.get_or_insert(true);
        i += 1;
        let value = if at(line, i) == b'"' {
            match quoted(line, i) {
                Some((s, e, _, next)) => {
                    i = next;
                    Value::Quoted(s, e)
                }
                None => {
                    // Unterminated: the rest of the line.
                    let s = i + 1;
                    i = n;
                    Value::Quoted(s.min(n), n)
                }
            }
        } else {
            let s = i;
            while i < n && !space(at(line, i)) {
                i += 1;
            }
            Value::Bare(s, i)
        };
        pairs += 1;
        emit(line, &mut ids, out, &mut has_msg, line.get(ks..ke).unwrap_or_default(), value);
    }
    if pairs == 0 {
        whole(out, line);
        return false;
    }
    if !has_msg {
        out.message = sp(0, line.len());
    }
    pairs >= 2 && first_is_pair == Some(true) && pairs >= bare
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Timestamp};

    fn fields(names: &FieldNames, line: &[u8], p: &Parsed) -> Vec<(String, String)> {
        p.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), LogfmtParser.field_text(line, *s))).collect()
    }

    #[test]
    fn table() {
        let n = FieldNames::default();
        // (line, time, level, message, fields, fits)
        let cases: &[(&str, Option<&str>, Option<Level>, &str, &[(&str, &str)], bool)] = &[
            (
                r#"time=2026-11-12T07:41:03.118Z level=error service=api msg="upstream timeout" request_id=r-1 status=503"#,
                Some("2026-11-12T07:41:03.118Z"),
                Some(Level::Error),
                "upstream timeout",
                &[("service", "api"), ("request_id", "r-1"), ("status", "503")],
                true,
            ),
            (
                r#"ts=1794469263 lvl=WARN msg=bare quote="a \"b\" c" empty="#,
                Some("2026-11-12T07:41:03.000Z"),
                Some(Level::Warn),
                "bare",
                &[("quote", "a \"b\" c"), ("empty", "")],
                true,
            ),
            ("a=1 b=2", None, None, "a=1 b=2", &[("a", "1"), ("b", "2")], true),
            ("hello world a=1", None, None, "hello world a=1", &[("a", "1")], false),
            ("a=1", None, None, "a=1", &[("a", "1")], false),
            (r#"msg="never closed a=1"#, None, None, "never closed a=1", &[], false),
            ("no pairs at all", None, None, "no pairs at all", &[], false),
            ("=x ==", None, None, "=x ==", &[], false),
        ];
        for (line, time, level, msg, want, fits) in cases {
            let b = line.as_bytes();
            let mut p = Parsed::default();
            assert_eq!(parse_line(b, &n, &mut p), *fits, "{line}");
            assert_eq!(p.time.map(Timestamp::to_rfc3339).as_deref(), *time, "{line}");
            assert_eq!(p.level, *level, "{line}");
            assert_eq!(LogfmtParser.field_text(b, p.message), *msg, "{line}");
            let want: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
            assert_eq!(fields(&n, b, &p), want, "{line}");
        }
    }
}
