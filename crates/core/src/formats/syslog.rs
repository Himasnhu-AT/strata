//! Syslog: RFC 5424 (`<14>1 2026-11-12T07:41:03Z host app 123 ID47 [sd k="v"] msg`) and RFC 3164
//! (`<14>Nov 12 07:41:03 host app[123]: msg`, the `<pri>` optional as files often drop it, the
//! year inferred). Fields `host`, `app`, `pid`, `msgid`, structured-data params by name and
//! `key=value` tokens in the message. The level comes from the priority's severity, or from a
//! leading level word in the message (`<Notice>`) when there is no priority.

use super::{FieldNames, FormatKind, Ids, Parser, at, cut, level_word, push_field, quoted, reset, scan_kv, sp, strip_word, syslog_severity, whole};
use crate::Parsed;
use crate::time::scan_timestamp;

/// RFC 3164 and RFC 5424.
#[derive(Debug, Default)]
pub struct SyslogParser;

impl Parser for SyslogParser {
    fn kind(&self) -> FormatKind {
        FormatKind::Syslog
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

/// The token from `i` to the next space (or the end): its end.
fn token_end(line: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < line.len() && at(line, j) != b' ' {
        j += 1;
    }
    j
}

/// Parse `line` into `out`; whether it looks like syslog.
pub(crate) fn parse_line(line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    let line = cut(line);
    reset(out);
    let mut i = 0;
    let mut severity = None;
    if at(line, 0) == b'<' {
        let mut j = 1;
        let mut pri: u32 = 0;
        while at(line, j).is_ascii_digit() && j <= 3 {
            pri = pri * 10 + u32::from(at(line, j) - b'0');
            j += 1;
        }
        if j == 1 || at(line, j) != b'>' || pri > 191 {
            whole(out, line);
            return false;
        }
        severity = Some(i64::from(pri % 8));
        i = j + 1;
    }
    let mut ids = Ids::new(names);
    let v = at(line, i);
    let is_5424 = severity.is_some()
        && (b'1'..=b'9').contains(&v)
        && (at(line, i + 1) == b' ' || (at(line, i + 1).is_ascii_digit() && at(line, i + 2) == b' '));
    let ok = if is_5424 { rfc5424(line, i, &mut ids, out) } else { rfc3164(line, i, severity.is_some(), &mut ids, out) };
    if !ok {
        drop(ids);
        whole(out, line);
        return false;
    }
    if let Some(sev) = severity {
        out.level = Some(syslog_severity(sev));
    }
    true
}

fn rfc5424(line: &[u8], mut i: usize, ids: &mut Ids<'_>, out: &mut Parsed) -> bool {
    i = token_end(line, i) + 1; // version
    if at(line, i) == b'-' {
        i += 1;
    } else {
        match scan_timestamp(line.get(i..).unwrap_or_default()) {
            Some((t, n)) => {
                out.time = Some(t);
                i += n;
            }
            None => return false,
        }
    }
    for name in [&b"host"[..], b"app", b"pid", b"msgid"] {
        if at(line, i) != b' ' {
            return false;
        }
        i += 1;
        let e = token_end(line, i);
        if e == i {
            return false;
        }
        if line.get(i..e) != Some(b"-") {
            push_field(out, ids, name, sp(i, e));
        }
        i = e;
    }
    if at(line, i) != b' ' {
        return false;
    }
    i += 1;
    // Structured data: `-` or `[id k="v" …]…`.
    if at(line, i) == b'-' {
        i += 1;
    } else if at(line, i) == b'[' {
        while at(line, i) == b'[' {
            match sd_element(line, i + 1, ids, out) {
                Some(next) => i = next,
                None => return false,
            }
        }
    } else {
        return false;
    }
    if at(line, i) == b' ' {
        i += 1;
    }
    if line.get(i..i + 3) == Some(b"\xEF\xBB\xBF") {
        i += 3;
    }
    out.message = sp(i, line.len());
    scan_kv(line, i, line.len(), ids, out);
    true
}

/// One `[id k="v" …]` from just after `[`; the index after `]`.
fn sd_element(line: &[u8], mut i: usize, ids: &mut Ids<'_>, out: &mut Parsed) -> Option<usize> {
    while !matches!(at(line, i), b' ' | b']' | 0) {
        i += 1;
    }
    loop {
        match at(line, i) {
            b']' => return Some(i + 1),
            b' ' => i += 1,
            0 => return None,
            _ => {
                let ks = i;
                while !matches!(at(line, i), b'=' | b' ' | b']' | 0) {
                    i += 1;
                }
                if at(line, i) != b'=' || at(line, i + 1) != b'"' {
                    return None;
                }
                let (s, e, _, next) = quoted(line, i + 1)?;
                push_field(out, ids, line.get(ks..i)?, sp(s, e));
                i = next;
            }
        }
    }
}

fn rfc3164(line: &[u8], mut i: usize, has_pri: bool, ids: &mut Ids<'_>, out: &mut Parsed) -> bool {
    let Some((t, n)) = scan_timestamp(line.get(i..).unwrap_or_default()) else {
        return false;
    };
    out.time = Some(t);
    i += n;
    if at(line, i) != b' ' {
        return false;
    }
    i += 1;
    let host_end = token_end(line, i);
    let host = line.get(i..host_end).unwrap_or_default();
    if host.is_empty() || host.first() == Some(&b'[') || level_word(strip_word(host)).is_some() {
        return false;
    }
    push_field(out, ids, b"host", sp(i, host_end));
    i = host_end + 1;
    // The tag: `app[123]:` or `app:`.
    let tag_end = token_end(line, i);
    let tag = line.get(i..tag_end).unwrap_or_default();
    let mut tagged = false;
    if tag.len() > 1 && tag.len() <= 64 && (tag.ends_with(b":") || tag.ends_with(b"]")) {
        let name_end = tag.iter().position(|&c| c == b'[' || c == b':').unwrap_or(tag.len());
        if name_end > 0 {
            tagged = true;
            push_field(out, ids, b"app", sp(i, i + name_end));
            if at(tag, name_end) == b'['
                && let Some(close) = tag.iter().position(|&c| c == b']')
                && close > name_end + 1
            {
                push_field(out, ids, b"pid", sp(i + name_end + 1, i + close));
            }
            i = tag_end;
            if at(line, i) == b' ' {
                i += 1;
            }
        }
    }
    if !tagged && !has_pri {
        return false;
    }
    let i = i.min(line.len());
    out.message = sp(i, line.len());
    if !has_pri {
        let w = token_end(line, i);
        out.level = level_word(strip_word(line.get(i..w).unwrap_or_default()));
    }
    scan_kv(line, i, line.len(), ids, out);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Timestamp};

    fn fields(names: &FieldNames, line: &[u8], p: &Parsed) -> Vec<(String, String)> {
        p.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), SyslogParser.field_text(line, *s))).collect()
    }

    #[test]
    fn table() {
        let n = FieldNames::default();
        // (line, time, level, message, fields, fits)
        let cases: &[(&str, Option<&str>, Option<Level>, &str, &[(&str, &str)], bool)] = &[
            (
                "<11>1 2026-11-12T07:41:03.118Z host-3 api 1003 - - upstream timeout request_id=r-0000beef",
                Some("2026-11-12T07:41:03.118Z"),
                Some(Level::Error),
                "upstream timeout request_id=r-0000beef",
                &[("host", "host-3"), ("app", "api"), ("pid", "1003"), ("request_id", "r-0000beef")],
                true,
            ),
            (
                r#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventSource="Appli\"cation"][x y="z"] An application event"#,
                Some("2003-10-11T22:14:15.003Z"),
                Some(Level::Info),
                "An application event",
                &[
                    ("host", "mymachine.example.com"),
                    ("app", "evntslog"),
                    ("msgid", "ID47"),
                    ("iut", "3"),
                    ("eventSource", "Appli\\\"cation"),
                    ("y", "z"),
                ],
                true,
            ),
            ("<14>1 - - - - - -", None, Some(Level::Info), "", &[], true),
            (
                "<34>Oct 11 22:14:15 mymachine su: 'su root' failed",
                Some("2025-10-11T22:14:15.000Z"),
                Some(Level::Fatal),
                "'su root' failed",
                &[("host", "mymachine"), ("app", "su")],
                true,
            ),
            (
                "Nov 12 07:41:03 web sshd[123]: Accepted key",
                Some("2026-11-12T07:41:03.000Z"),
                None,
                "Accepted key",
                &[("host", "web"), ("app", "sshd"), ("pid", "123")],
                true,
            ),
            (
                "Nov 12 07:41:03 mac kernel[0] <Error>: bad",
                Some("2026-11-12T07:41:03.000Z"),
                Some(Level::Error),
                "<Error>: bad",
                &[("host", "mac"), ("app", "kernel"), ("pid", "0")],
                true,
            ),
            (
                "2026-11-12T07:41:03.118+00:00 web app[7]: hi",
                Some("2026-11-12T07:41:03.118Z"),
                None,
                "hi",
                &[("host", "web"), ("app", "app"), ("pid", "7")],
                true,
            ),
            // not syslog
            ("Nov 12 07:41:03 INFO api: hi", None, None, "Nov 12 07:41:03 INFO api: hi", &[], false),
            ("<999>1 x", None, None, "<999>1 x", &[], false),
            ("<14>1 notatime h a p m - x", None, None, "<14>1 notatime h a p m - x", &[], false),
            ("<14>1 2026-11-12T07:41:03Z h a p m [bad x", None, None, "<14>1 2026-11-12T07:41:03Z h a p m [bad x", &[], false),
            ("<>", None, None, "<>", &[], false),
            ("plain words", None, None, "plain words", &[], false),
        ];
        for (line, time, level, msg, want, fits) in cases {
            let b = line.as_bytes();
            let mut p = Parsed::default();
            assert_eq!(parse_line(b, &n, &mut p), *fits, "{line}");
            let year_free = p.time.map(Timestamp::to_rfc3339);
            if line.starts_with("<34>Oct") || line.starts_with("Nov") {
                // Year inferred from the clock: compare without it.
                assert_eq!(year_free.as_deref().map(|s| &s[4..]), time.map(|s| &s[4..]), "{line}");
            } else {
                assert_eq!(year_free.as_deref(), *time, "{line}");
            }
            assert_eq!(p.level, *level, "{line}");
            assert_eq!(SyslogParser.field_text(b, p.message), *msg, "{line}");
            let want: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
            assert_eq!(fields(&n, b, &p), want, "{line}");
        }
        assert_eq!(syslog_severity(7), Level::Debug);
    }
}
