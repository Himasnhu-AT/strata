//! Text starting with a timestamp, optionally in brackets, usually followed by a level:
//! `[2026-11-12 07:41:03.118] INFO  api: request handled`, `2026-11-12 07:41:03,118 ERROR [main]
//! c.x.Y - boom`, `2026-11-12 07:41:03,118 - name - WARNING - msg`, `1794469263.118 DEBUG x`.
//!
//! The level is the first token after the time, or the second or third when it is bracketed or
//! uppercase (so `… Connection error occurred` is not an error). A `name:` token right after the
//! level is the `logger` field. The message is the rest; `key=value` tokens in it are fields.
//! Lines without a leading time (stack traces) are whole-line messages.

use super::{FieldNames, FormatKind, Ids, Parser, at, cut, level_word, push_field, reset, scan_kv, sp, strip_word, whole};
use crate::Parsed;
use crate::time::scan_timestamp;

/// Timestamp-prefixed text.
#[derive(Debug, Default)]
pub struct TimestampedParser;

impl Parser for TimestampedParser {
    fn kind(&self) -> FormatKind {
        FormatKind::Timestamped
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

/// Spaces and separators between the parts: ` `, `\t`, `|`, and `-` or `:` followed by a space.
fn skip_seps(line: &[u8], mut i: usize) -> usize {
    loop {
        match at(line, i) {
            b' ' | b'\t' | b'|' => i += 1,
            b'-' | b':' if matches!(at(line, i + 1), b' ' | b'\t') => i += 1,
            _ => return i,
        }
    }
}

fn token_end(line: &[u8], mut i: usize) -> usize {
    while i < line.len() && !matches!(at(line, i), b' ' | b'\t') {
        i += 1;
    }
    i
}

/// Parse `line` into `out`; whether it starts with a timestamp.
pub(crate) fn parse_line(line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    let line = cut(line);
    reset(out);
    let open = at(line, 0);
    let mut i = usize::from(matches!(open, b'[' | b'('));
    let Some((t, n)) = scan_timestamp(line.get(i..).unwrap_or_default()) else {
        whole(out, line);
        return false;
    };
    out.time = Some(t);
    i += n;
    if (open == b'[' && at(line, i) == b']') || (open == b'(' && at(line, i) == b')') {
        i += 1;
    }
    let after_time = i;
    let mut msg = skip_seps(line, after_time);
    let mut j = msg;
    for k in 0..3 {
        j = skip_seps(line, j);
        let e = token_end(line, j);
        if e == j {
            break;
        }
        let raw = line.get(j..e).unwrap_or_default();
        let word = strip_word(raw);
        let marked = word.len() != raw.len() || word.iter().all(|c| !c.is_ascii_lowercase());
        if (k == 0 || marked)
            && let Some(level) = level_word(word)
        {
            out.level = Some(level);
            msg = skip_seps(line, e);
            break;
        }
        j = e;
    }
    let mut ids = Ids::new(names);
    if out.level.is_some() {
        // `api: msg` → logger `api`.
        let e = token_end(line, msg);
        let tok = line.get(msg..e).unwrap_or_default();
        if tok.len() >= 2 && tok.len() <= 65 && tok.ends_with(b":") && e < line.len() {
            let name = tok.get(..tok.len() - 1).unwrap_or_default();
            if name.iter().all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'/')) {
                push_field(out, &mut ids, b"logger", sp(msg, e - 1));
                msg = skip_seps(line, e);
            }
        }
    }
    let msg = msg.min(line.len());
    out.message = sp(msg, line.len());
    scan_kv(line, msg, line.len(), &mut ids, out);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Timestamp};

    fn fields(names: &FieldNames, line: &[u8], p: &Parsed) -> Vec<(String, String)> {
        p.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), TimestampedParser.field_text(line, *s))).collect()
    }

    #[test]
    fn table() {
        let n = FieldNames::default();
        let cases: &[(&str, Option<&str>, Option<Level>, &str, &[(&str, &str)])] = &[
            (
                "[2026-11-12 00:00:00.035] INFO  api: request handled (request r-0000beef, 12 ms)",
                Some("2026-11-12T00:00:00.035Z"),
                Some(Level::Info),
                "request handled (request r-0000beef, 12 ms)",
                &[("logger", "api")],
            ),
            (
                "2026-11-12 07:41:03,118 ERROR [main] c.x.Y - boom id=7",
                Some("2026-11-12T07:41:03.118Z"),
                Some(Level::Error),
                "[main] c.x.Y - boom id=7",
                &[("id", "7")],
            ),
            ("2026-11-12 07:41:03,118 - app.db - WARNING - slow", Some("2026-11-12T07:41:03.118Z"), Some(Level::Warn), "slow", &[]),
            ("2026-11-12T07:41:03Z [main] [ERROR] x", Some("2026-11-12T07:41:03.000Z"), Some(Level::Error), "x", &[]),
            ("2026-11-12T07:41:03Z Connection error occurred", Some("2026-11-12T07:41:03.000Z"), None, "Connection error occurred", &[]),
            ("1794469263.118 DEBUG x", Some("2026-11-12T07:41:03.118Z"), Some(Level::Debug), "x", &[]),
            ("12-Nov-2026 07:41:03.118 SEVERE [main] boom", Some("2026-11-12T07:41:03.118Z"), Some(Level::Error), "[main] boom", &[]),
            ("2026-11-12T07:41:03Z", Some("2026-11-12T07:41:03.000Z"), None, "", &[]),
            ("2026-11-12T07:41:03Z WARN", Some("2026-11-12T07:41:03.000Z"), Some(Level::Warn), "", &[]),
            ("2026-11-12T07:41:03Z warn: lower", Some("2026-11-12T07:41:03.000Z"), Some(Level::Warn), "lower", &[]),
            ("Traceback (most recent call last):", None, None, "Traceback (most recent call last):", &[]),
            ("[not a time] x", None, None, "[not a time] x", &[]),
        ];
        for (line, time, level, msg, want) in cases {
            let b = line.as_bytes();
            let p = TimestampedParser.parse(b, &n);
            assert_eq!(p.time.map(Timestamp::to_rfc3339).as_deref(), *time, "{line}");
            assert_eq!(p.level, *level, "{line}");
            assert_eq!(TimestampedParser.field_text(b, p.message), *msg, "{line}");
            let want: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
            assert_eq!(fields(&n, b, &p), want, "{line}");
        }
    }
}
