//! Apache / nginx access logs, common and combined:
//! `client ident user [12/Nov/2026:07:41:03 +0000] "GET /path HTTP/1.1" status bytes "referer" "agent" …`.
//! Fields `client`, `ident`, `user`, `method`, `path`, `protocol`, `status`, `bytes`, `referer`,
//! `user_agent` (`-` means absent), then trailing tokens: `key=value` by key, others `extra1`…
//! The message is the request line; the level follows the status (5xx error, 4xx warn, else
//! info).

use super::{FieldNames, FormatKind, Ids, Parser, at, cut, push_field, quoted, reset, sp, whole};
use crate::time::scan_timestamp;
use crate::{Level, Parsed};

/// Common and combined access logs.
#[derive(Debug, Default)]
pub struct AccessParser;

impl Parser for AccessParser {
    fn kind(&self) -> FormatKind {
        FormatKind::AccessLog
    }

    fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed {
        let mut p = Parsed::default();
        self.parse_into(line, names, &mut p);
        p
    }

    fn parse_into(&self, line: &[u8], names: &FieldNames, out: &mut Parsed) {
        parse_line(line, names, out);
    }
}

const EXTRAS: [&[u8]; 8] = [b"extra1", b"extra2", b"extra3", b"extra4", b"extra5", b"extra6", b"extra7", b"extra8"];

fn token_end(line: &[u8], mut i: usize) -> usize {
    while i < line.len() && at(line, i) != b' ' {
        i += 1;
    }
    i
}

/// Parse `line` into `out`; whether it is an access log line.
pub(crate) fn parse_line(line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    let line = cut(line);
    reset(out);
    let mut ids = Ids::new(names);
    if parse_parts(line, &mut ids, out).is_none() {
        drop(ids);
        whole(out, line);
        return false;
    }
    true
}

fn parse_parts(line: &[u8], ids: &mut Ids<'_>, out: &mut Parsed) -> Option<()> {
    let mut i = 0;
    for name in [&b"client"[..], b"ident", b"user"] {
        let e = token_end(line, i);
        if e == i || at(line, e) != b' ' {
            return None;
        }
        if line.get(i..e) != Some(b"-") {
            push_field(out, ids, name, sp(i, e));
        }
        i = e + 1;
    }
    if at(line, i) != b'[' {
        return None;
    }
    let close = i + 1 + memchr::memchr(b']', line.get(i + 1..)?)?;
    let (t, _) = scan_timestamp(line.get(i + 1..close)?)?;
    out.time = Some(t);
    i = close + 1;
    if at(line, i) != b' ' || at(line, i + 1) != b'"' {
        return None;
    }
    let (rs, re, _, next) = quoted(line, i + 1)?;
    out.message = sp(rs, re);
    let request = line.get(rs..re)?;
    let m = request.iter().position(|&c| c == b' ').unwrap_or(request.len());
    if m < request.len() && m > 0 {
        push_field(out, ids, b"method", sp(rs, rs + m));
        let p_end = request.iter().rposition(|&c| c == b' ').unwrap_or(m);
        let has_proto = p_end > m && request.get(p_end + 1..).is_some_and(|p| p.starts_with(b"HTTP/"));
        let path_end = if has_proto { p_end } else { request.len() };
        if path_end > m + 1 {
            push_field(out, ids, b"path", sp(rs + m + 1, rs + path_end));
        }
        if has_proto {
            push_field(out, ids, b"protocol", sp(rs + p_end + 1, re));
        }
    }
    i = next;
    if at(line, i) != b' ' {
        return None;
    }
    i += 1;
    let status = line.get(i..i + 3)?;
    if !status.iter().all(u8::is_ascii_digit) || !matches!(at(line, i + 3), b' ' | 0) {
        return None;
    }
    push_field(out, ids, b"status", sp(i, i + 3));
    out.level = Some(match status.first() {
        Some(b'5') => Level::Error,
        Some(b'4') => Level::Warn,
        _ => Level::Info,
    });
    i += 3;
    if at(line, i) == b' ' {
        let e = token_end(line, i + 1);
        let bytes = line.get(i + 1..e)?;
        if bytes != b"-" && !bytes.iter().all(u8::is_ascii_digit) {
            return None;
        }
        if bytes != b"-" && !bytes.is_empty() {
            push_field(out, ids, b"bytes", sp(i + 1, e));
        }
        i = e;
    }
    // Combined: "referer" "agent"; then anything else.
    let mut extra = 0;
    let mut quoted_seen = 0;
    while at(line, i) == b' ' {
        i += 1;
        let c = at(line, i);
        if c == 0 {
            break;
        }
        if c == b'"' {
            let (s, e, _, n) = quoted(line, i)?;
            let name = match quoted_seen {
                0 => Some(&b"referer"[..]),
                1 => Some(&b"user_agent"[..]),
                _ => None,
            };
            quoted_seen += 1;
            let blank = line.get(s..e) == Some(b"-");
            match name {
                Some(name) if !blank => push_field(out, ids, name, sp(s, e)),
                Some(_) => {}
                None => {
                    if let Some(name) = EXTRAS.get(extra) {
                        push_field(out, ids, name, sp(s, e));
                    }
                    extra += 1;
                }
            }
            i = n;
            continue;
        }
        quoted_seen = 2;
        let e = token_end(line, i);
        let tok = line.get(i..e)?;
        match tok.iter().position(|&c| c == b'=') {
            Some(k) if k > 0 => push_field(out, ids, tok.get(..k)?, sp(i + k + 1, e)),
            _ => {
                if let Some(name) = EXTRAS.get(extra) {
                    push_field(out, ids, name, sp(i, e));
                }
                extra += 1;
            }
        }
        i = e;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;

    fn fields(names: &FieldNames, line: &[u8], p: &Parsed) -> Vec<(String, String)> {
        p.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), AccessParser.field_text(line, *s))).collect()
    }

    #[test]
    fn table() {
        let n = FieldNames::default();
        let cases: &[(&str, Option<&str>, Option<Level>, &str, &[(&str, &str)], bool)] = &[
            (
                r#"10.0.12.7 - - [12/Nov/2026:07:41:03 +0000] "GET /v1/orders/42 HTTP/1.1" 503 1200 "-" "strata-fixture/1.0" 312"#,
                Some("2026-11-12T07:41:03.000Z"),
                Some(Level::Error),
                "GET /v1/orders/42 HTTP/1.1",
                &[
                    ("client", "10.0.12.7"),
                    ("method", "GET"),
                    ("path", "/v1/orders/42"),
                    ("protocol", "HTTP/1.1"),
                    ("status", "503"),
                    ("bytes", "1200"),
                    ("user_agent", "strata-fixture/1.0"),
                    ("extra1", "312"),
                ],
                true,
            ),
            (
                r#"127.0.0.1 user-identifier frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 404 -"#,
                Some("2000-10-10T20:55:36.000Z"),
                Some(Level::Warn),
                "GET /apache_pb.gif HTTP/1.0",
                &[
                    ("client", "127.0.0.1"),
                    ("ident", "user-identifier"),
                    ("user", "frank"),
                    ("method", "GET"),
                    ("path", "/apache_pb.gif"),
                    ("protocol", "HTTP/1.0"),
                    ("status", "404"),
                ],
                true,
            ),
            (
                r#"::1 - - [12/Nov/2026:07:41:03 +0000] "-" 400 0 "http://x/" "ua \"q\"" rt=0.012 "z""#,
                Some("2026-11-12T07:41:03.000Z"),
                Some(Level::Warn),
                "-",
                &[
                    ("client", "::1"),
                    ("status", "400"),
                    ("bytes", "0"),
                    ("referer", "http://x/"),
                    ("user_agent", "ua \\\"q\\\""),
                    ("rt", "0.012"),
                    ("extra1", "z"),
                ],
                true,
            ),
            ("10.0.0.1 - - [nope] \"GET / HTTP/1.1\" 200 1", None, None, "10.0.0.1 - - [nope] \"GET / HTTP/1.1\" 200 1", &[], false),
            (
                "10.0.0.1 - - [12/Nov/2026:07:41:03 +0000] \"GET / HTTP/1.1\" 2000 1",
                None,
                None,
                "10.0.0.1 - - [12/Nov/2026:07:41:03 +0000] \"GET / HTTP/1.1\" 2000 1",
                &[],
                false,
            ),
            ("just text", None, None, "just text", &[], false),
            ("a b c [", None, None, "a b c [", &[], false),
        ];
        for (line, time, level, msg, want, fits) in cases {
            let b = line.as_bytes();
            let mut p = Parsed::default();
            assert_eq!(parse_line(b, &n, &mut p), *fits, "{line}");
            assert_eq!(p.time.map(Timestamp::to_rfc3339).as_deref(), *time, "{line}");
            assert_eq!(p.level, *level, "{line}");
            assert_eq!(AccessParser.field_text(b, p.message), *msg, "{line}");
            let want: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
            assert_eq!(fields(&n, b, &p), want, "{line}");
        }
    }
}
