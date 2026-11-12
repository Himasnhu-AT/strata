//! Times and durations. Owned by the `formats` stream. Formatting is [`Timestamp::to_rfc3339`].
//!
//! Two kinds of input:
//!
//! - **typed by people and agents** (`view.goto`, the filter grammar, `log.histogram`'s
//!   `bucket`): [`parse_time`], [`parse_duration`];
//! - **written by logs**, found at the start of some bytes: [`scan_timestamp`] (ISO 8601 / RFC 3339
//!   variants, Apache `12/Nov/2026:07:41:03 +0000`, syslog `Nov 12 07:41:03` with the year
//!   inferred, `12-Nov-2026 07:41:03`, epoch seconds / millis / micros / nanos) and
//!   [`epoch_number`] (a JSON or logfmt number).
//!
//! Everything works on bytes, never allocates and never panics: a log is hostile input.

use std::sync::OnceLock;

use crate::{Error, Result, Timestamp};

const MICROS: i64 = Timestamp::MICROS_PER_SECOND;
const MONTHS: [&[u8; 3]; 12] = [b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov", b"dec"];

/// Parse a time: RFC 3339 / ISO 8601 (`2026-11-12T07:41:03.118Z`, `2026-11-12 07:41:03`, a
/// missing zone means UTC), a date alone (`2026-11-12`, midnight UTC), or epoch seconds /
/// milliseconds as an integer. The log forms of [`scan_timestamp`] are accepted too.
pub fn parse_time(text: &str) -> Result<Timestamp> {
    let t = text.trim();
    let b = t.as_bytes();
    let whole = |r: Option<(Timestamp, usize)>| r.filter(|(_, n)| *n == b.len()).map(|(ts, _)| ts);
    if let Some(ts) = whole(scan_iso(b, false)) {
        return Ok(ts);
    }
    if let Some(ts) = whole(scan_timestamp(b)) {
        return Ok(ts);
    }
    // A bare integer: guess the unit from its size.
    let (neg, digits) = match b.first() {
        Some(b'-') => (true, b.get(1..).unwrap_or_default()),
        _ => (false, b),
    };
    if !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && let Some(ts) = epoch_number(digits)
    {
        return Ok(if neg { Timestamp(ts.0.saturating_neg()) } else { ts });
    }
    Err(Error::BadTime { text: text.to_string() })
}

/// Parse a duration: `<integer><unit>` with unit `us`, `ms`, `s`, `m`, `h` or `d` (`250ms`,
/// `5m`), optionally negative, in microseconds.
pub fn parse_duration(text: &str) -> Result<i64> {
    let bad = || Error::BadTime { text: text.to_string() };
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let split = t.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (num, unit) = t.split_at(split);
    if num.is_empty() {
        return Err(bad());
    }
    let n: i64 = num.parse().map_err(|_| bad())?;
    let mult: i64 = match unit.trim() {
        "us" | "µs" => 1,
        "ms" => 1000,
        "s" => MICROS,
        "m" => 60 * MICROS,
        "h" => 3600 * MICROS,
        "d" => 86_400 * MICROS,
        _ => return Err(bad()),
    };
    let v = n.checked_mul(mult).ok_or_else(bad)?;
    Ok(if neg { -v } else { v })
}

/// Format a duration in microseconds the way [`parse_duration`] reads it, with the largest unit
/// that divides it (`300_000_000` → `5m`, `0` → `0s`).
pub fn format_duration(micros: i64) -> String {
    if micros == 0 {
        return "0s".to_string();
    }
    const UNITS: [(i64, &str); 5] = [(86_400 * MICROS, "d"), (3600 * MICROS, "h"), (60 * MICROS, "m"), (MICROS, "s"), (1000, "ms")];
    for (size, unit) in UNITS {
        if micros % size == 0 {
            return format!("{}{unit}", micros / size);
        }
    }
    format!("{micros}us")
}

/// The timestamp at the start of `b`, and how many bytes it took. Recognises ISO 8601 / RFC 3339
/// (`2026-11-12T07:41:03.118Z`, `2026-11-12 07:41:03,118`, `2026/11/12 07:41:03 +01:00`, `… UTC`),
/// Apache (`12/Nov/2026:07:41:03 +0000`), syslog (`Nov 12 07:41:03`, year inferred from now),
/// `12-Nov-2026 07:41:03.118` and epoch numbers of 10 to 19 digits (`1794469263.118`). A missing
/// zone means UTC.
pub fn scan_timestamp(b: &[u8]) -> Option<(Timestamp, usize)> {
    scan_timestamp_near(b, now())
}

/// Like [`scan_timestamp`], inferring a missing year (syslog) from `now` instead of the clock:
/// the year that puts the time at most a week after `now` (pass a file's modification time).
pub fn scan_timestamp_near(b: &[u8], now: Timestamp) -> Option<(Timestamp, usize)> {
    let c0 = at(b, 0);
    if c0.is_ascii_digit() {
        if at(b, 4) == b'-' || at(b, 4) == b'/' {
            return scan_iso(b, true);
        }
        if at(b, 2) == b'/' {
            return scan_apache(b);
        }
        if at(b, 2) == b'-' || at(b, 1) == b'-' {
            return scan_dmy(b);
        }
        return scan_epoch_prefix(b);
    }
    if c0.is_ascii_alphabetic() {
        return scan_syslog(b, now);
    }
    None
}

/// A number written as epoch time (`1794469263`, `1794469263118`, `1794469263.118`): the unit is
/// guessed from the digits before the point (up to 11: seconds, 12–14: millis, 15–17: micros,
/// 18–19: nanos). `None` for anything else, negative numbers included.
pub fn epoch_number(b: &[u8]) -> Option<Timestamp> {
    let int_len = b.iter().position(|c| !c.is_ascii_digit()).unwrap_or(b.len());
    if int_len == 0 || int_len > 19 {
        return None;
    }
    let mut int: i64 = 0;
    for &c in b.get(..int_len)? {
        int = int.checked_mul(10)?.checked_add(i64::from(c - b'0'))?;
    }
    let rest = b.get(int_len..).unwrap_or_default();
    let frac = match rest.first() {
        None => &[][..],
        Some(b'.') => {
            let f = rest.get(1..).unwrap_or_default();
            if f.is_empty() || !f.iter().all(u8::is_ascii_digit) {
                return None;
            }
            f
        }
        Some(_) => return None,
    };
    // Scale to micros: the integer times 10^k plus the fraction's first k digits.
    let k: usize = match int_len {
        0..=11 => 6,
        12..=14 => 3,
        15..=17 => 0,
        _ => return Some(Timestamp(int / 1000)),
    };
    let mut f: i64 = 0;
    for i in 0..k {
        f = f * 10 + frac.get(i).map_or(0, |c| i64::from(c - b'0'));
    }
    let scale = 10i64.pow(u32::try_from(k).unwrap_or(0));
    Some(Timestamp(int.checked_mul(scale)?.checked_add(f)?))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// A civil date and time plus an offset east of UTC in seconds, as UTC micros. `None` when a
/// field is out of range (second 60, a leap second, is read as 59).
fn civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32, micros: i64, offset_secs: i64) -> Option<Timestamp> {
    if !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let s = s.min(59);
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s) - offset_secs;
    Some(Timestamp(secs.checked_mul(MICROS)?.checked_add(micros)?))
}

fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

/// Exactly `n` digits at `i`.
fn digits(b: &[u8], i: usize, n: usize) -> Option<u32> {
    let mut v: u32 = 0;
    for k in 0..n {
        let c = at(b, i + k);
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(c - b'0');
    }
    Some(v)
}

/// A fraction of a second after `.` or `,` at `i` (digits past the sixth are read and dropped):
/// (micros, bytes taken including the separator).
fn fraction(b: &[u8], i: usize) -> (i64, usize) {
    if !matches!(at(b, i), b'.' | b',') || !at(b, i + 1).is_ascii_digit() {
        return (0, 0);
    }
    let mut micros: i64 = 0;
    let mut n = 0;
    while at(b, i + 1 + n).is_ascii_digit() && n < 32 {
        if n < 6 {
            micros = micros * 10 + i64::from(at(b, i + 1 + n) - b'0');
        }
        n += 1;
    }
    for _ in n..6 {
        micros *= 10;
    }
    (micros, n + 1)
}

/// A zone at `i`: `Z`, `±HH`, `±HHMM`, `±HH:MM`, optionally after one space; ` UTC` / ` GMT`.
/// (offset seconds east, bytes taken).
fn zone(b: &[u8], i: usize) -> (i64, usize) {
    match at(b, i) {
        b'Z' | b'z' => return (0, 1),
        b'+' | b'-' => {
            if let Some(r) = offset(b, i) {
                return r;
            }
        }
        b' ' => {
            if matches!(at(b, i + 1), b'+' | b'-')
                && let Some((o, n)) = offset(b, i + 1)
            {
                // `07:41:03 -0500` but not `07:41:03 - 12 users`: four digits after a space.
                if n >= 5 {
                    return (o, n + 1);
                }
            }
            let w = b.get(i + 1..i + 4).unwrap_or_default();
            if (w.eq_ignore_ascii_case(b"UTC") || w.eq_ignore_ascii_case(b"GMT")) && !at(b, i + 4).is_ascii_alphanumeric() {
                return (0, 4);
            }
        }
        _ => {}
    }
    (0, 0)
}

fn offset(b: &[u8], i: usize) -> Option<(i64, usize)> {
    let sign: i64 = if at(b, i) == b'-' { -1 } else { 1 };
    let hh = digits(b, i + 1, 2)?;
    let (mm, n) = if at(b, i + 3) == b':' {
        (digits(b, i + 4, 2)?, 6)
    } else if let Some(mm) = digits(b, i + 3, 2) {
        (mm, 5)
    } else {
        (0, 3)
    };
    if hh > 23 || mm > 59 || at(b, i + n).is_ascii_digit() {
        return None;
    }
    Some((sign * (i64::from(hh) * 3600 + i64::from(mm) * 60), n))
}

/// `HH:MM[:SS][.frac]` at `i`: (h, m, s, micros, bytes).
fn clock(b: &[u8], i: usize) -> Option<(u32, u32, u32, i64, usize)> {
    let h = digits(b, i, 2)?;
    if at(b, i + 2) != b':' {
        return None;
    }
    let mi = digits(b, i + 3, 2)?;
    let mut n = 5;
    let mut s = 0;
    if at(b, i + 5) == b':' {
        s = digits(b, i + 6, 2)?;
        n = 8;
    }
    let (micros, f) = fraction(b, i + n);
    Some((h, mi, s, micros, n + f))
}

/// ISO 8601: `YYYY-MM-DD` or `YYYY/MM/DD`, then `T`/`t`/` `/`_` and a clock, then a zone.
/// Without `need_time` a date alone is midnight UTC.
fn scan_iso(b: &[u8], need_time: bool) -> Option<(Timestamp, usize)> {
    let y = digits(b, 0, 4)?;
    let sep = at(b, 4);
    if !matches!(sep, b'-' | b'/') || at(b, 7) != sep {
        return None;
    }
    let mo = digits(b, 5, 2)?;
    let d = digits(b, 8, 2)?;
    let mut i = 10;
    let time = if matches!(at(b, i), b'T' | b't' | b' ' | b'_') { clock(b, i + 1) } else { None };
    let Some((h, mi, s, micros, n)) = time else {
        if need_time || at(b, i).is_ascii_alphanumeric() {
            return None;
        }
        return civil(i64::from(y), mo, d, 0, 0, 0, 0, 0).map(|ts| (ts, i));
    };
    i += 1 + n;
    let (off, z) = zone(b, i);
    civil(i64::from(y), mo, d, h, mi, s, micros, off).map(|ts| (ts, i + z))
}

fn month(b: &[u8], i: usize) -> Option<u32> {
    let w = [at(b, i).to_ascii_lowercase(), at(b, i + 1).to_ascii_lowercase(), at(b, i + 2).to_ascii_lowercase()];
    MONTHS.iter().position(|m| **m == w).and_then(|p| u32::try_from(p + 1).ok())
}

/// Apache / nginx: `12/Nov/2026:07:41:03 +0000`.
fn scan_apache(b: &[u8]) -> Option<(Timestamp, usize)> {
    let d = digits(b, 0, 2)?;
    let mo = month(b, 3)?;
    if at(b, 2) != b'/' || at(b, 6) != b'/' || at(b, 11) != b':' {
        return None;
    }
    let y = digits(b, 7, 4)?;
    let (h, mi, s, micros, n) = clock(b, 12)?;
    let i = 12 + n;
    let (off, z) = zone(b, i);
    civil(i64::from(y), mo, d, h, mi, s, micros, off).map(|ts| (ts, i + z))
}

/// `12-Nov-2026 07:41:03.118` (Tomcat) or `2-Nov-2026 …`.
fn scan_dmy(b: &[u8]) -> Option<(Timestamp, usize)> {
    let (d, i) = if at(b, 1) == b'-' { (digits(b, 0, 1)?, 2) } else { (digits(b, 0, 2)?, 3) };
    let mo = month(b, i)?;
    if at(b, i + 3) != b'-' {
        return None;
    }
    let y = digits(b, i + 4, 4)?;
    if !matches!(at(b, i + 8), b' ' | b':' | b'T') {
        return None;
    }
    let (h, mi, s, micros, n) = clock(b, i + 9)?;
    let end = i + 9 + n;
    let (off, z) = zone(b, end);
    civil(i64::from(y), mo, d, h, mi, s, micros, off).map(|ts| (ts, end + z))
}

/// Syslog (RFC 3164): `Nov 12 07:41:03`, `Nov  2 07:41:03.118`; the year is the one that puts the
/// time at most a week after `now`. An optional `2026 ` after the day is read as the year.
fn scan_syslog(b: &[u8], now: Timestamp) -> Option<(Timestamp, usize)> {
    let mo = month(b, 0)?;
    if at(b, 3) != b' ' {
        return None;
    }
    let mut i = 4;
    if at(b, i) == b' ' {
        i += 1;
    }
    let d = if at(b, i + 1).is_ascii_digit() {
        let d = digits(b, i, 2)?;
        i += 2;
        d
    } else {
        let d = digits(b, i, 1)?;
        i += 1;
        d
    };
    if at(b, i) != b' ' {
        return None;
    }
    i += 1;
    let mut year = None;
    if let (Some(y), b' ') = (digits(b, i, 4), at(b, i + 4)) {
        year = Some(i64::from(y));
        i += 5;
    }
    let (h, mi, s, micros, n) = clock(b, i)?;
    i += n;
    let (off, z) = zone(b, i);
    let ts = match year {
        Some(y) => civil(y, mo, d, h, mi, s, micros, off)?,
        None => {
            let now_year = year_of(now);
            let this = civil(now_year, mo, d, h, mi, s, micros, off);
            match this {
                Some(t) if t.0 <= now.0.saturating_add(7 * 86_400 * MICROS) => t,
                // In the future, or Feb 29 in a year without one: an earlier year.
                _ => (1..=7).find_map(|back| civil(now_year - back, mo, d, h, mi, s, micros, off)).or(this)?,
            }
        }
    };
    Some((ts, i + z))
}

/// An epoch number of 10 to 19 digits at the start of a line, optionally with a fraction,
/// followed by a non-alphanumeric byte.
fn scan_epoch_prefix(b: &[u8]) -> Option<(Timestamp, usize)> {
    let int_len = b.iter().take(20).position(|c| !c.is_ascii_digit()).unwrap_or(b.len().min(20));
    if !(10..=19).contains(&int_len) {
        return None;
    }
    let mut end = int_len;
    if at(b, end) == b'.' && at(b, end + 1).is_ascii_digit() {
        end += 1;
        while at(b, end).is_ascii_digit() && end < int_len + 11 {
            end += 1;
        }
    }
    if at(b, end).is_ascii_alphanumeric() {
        return None;
    }
    epoch_number(b.get(..end)?).map(|ts| (ts, end))
}

fn year_of(ts: Timestamp) -> i64 {
    let days = ts.0.div_euclid(86_400 * MICROS);
    // The inverse of days_from_civil, year only.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let y = yoe + era * 400;
    if mp >= 10 { y + 1 } else { y }
}

/// The clock, read once per process (used only to infer syslog's missing year).
fn now() -> Timestamp {
    static NOW: OnceLock<Timestamp> = OnceLock::new();
    *NOW.get_or_init(|| {
        let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Timestamp(i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfc(ts: Timestamp) -> String {
        ts.to_rfc3339()
    }

    fn scan(s: &str) -> Option<(String, usize)> {
        // now = 2026-11-20T00:00:00Z
        scan_timestamp_near(s.as_bytes(), Timestamp(1_795_132_800_000_000)).map(|(t, n)| (rfc(t), n))
    }

    #[test]
    fn people_times_parse() {
        let ok = |s: &str| rfc(parse_time(s).unwrap());
        assert_eq!(ok("2026-11-12T07:41:03.118Z"), "2026-11-12T07:41:03.118Z");
        assert_eq!(ok("2026-11-12 07:41:03"), "2026-11-12T07:41:03.000Z");
        assert_eq!(ok("2026-11-12T07:41:03+02:00"), "2026-11-12T05:41:03.000Z");
        assert_eq!(ok("2026-11-12T07:41"), "2026-11-12T07:41:00.000Z");
        assert_eq!(ok(" 2026-11-12 "), "2026-11-12T00:00:00.000Z");
        assert_eq!(ok("1794469263"), "2026-11-12T07:41:03.000Z");
        assert_eq!(ok("1794469263118"), "2026-11-12T07:41:03.118Z");
        assert_eq!(ok("0"), "1970-01-01T00:00:00.000Z");
        for bad in ["", "yesterday", "2026-13-01", "2026-02-30", "2026-11-12T25:00", "2026-11-12Tx", "12345678901234567890123", "-"] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn durations_round_trip() {
        assert_eq!(parse_duration("250ms").unwrap(), 250_000);
        assert_eq!(parse_duration("5m").unwrap(), 300_000_000);
        assert_eq!(parse_duration("1d").unwrap(), 86_400_000_000);
        assert_eq!(parse_duration("-2s").unwrap(), -2_000_000);
        assert_eq!(parse_duration("7us").unwrap(), 7);
        for bad in ["", "5", "m", "5x", "1.5s", "99999999999999999999d", "9223372036854775807d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        for v in [0, 7, 1000, 250_000, 1_000_000, 300_000_000, 5_400_000_000, 86_400_000_000, -60_000_000, i64::MAX, i64::MIN + 1] {
            assert_eq!(parse_duration(&format_duration(v)).unwrap(), v, "{v}");
        }
        assert_eq!(format_duration(300_000_000), "5m");
        assert_eq!(format_duration(1_500_000), "1500ms");
    }

    #[test]
    fn log_timestamps_scan() {
        let cases: &[(&str, &str, usize)] = &[
            ("2026-11-12T07:41:03.118Z rest", "2026-11-12T07:41:03.118Z", 24),
            ("2026-11-12T07:41:03.118123456Z", "2026-11-12T07:41:03.118Z", 30),
            ("2026-11-12 07:41:03,118 INFO", "2026-11-12T07:41:03.118Z", 23),
            ("2026/11/12 07:41:03 x", "2026-11-12T07:41:03.000Z", 19),
            ("2026-11-12T07:41:03-05:00", "2026-11-12T12:41:03.000Z", 25),
            ("2026-11-12T07:41:03+0530", "2026-11-12T02:11:03.000Z", 24),
            ("2026-11-12 07:41:03 -0500 x", "2026-11-12T12:41:03.000Z", 25),
            ("2026-11-12 07:41:03 - x", "2026-11-12T07:41:03.000Z", 19),
            ("2026-11-12 07:41:03 UTC", "2026-11-12T07:41:03.000Z", 23),
            ("12/Nov/2026:07:41:03 +0000", "2026-11-12T07:41:03.000Z", 26),
            ("12/Nov/2026:07:41:03 -0700]", "2026-11-12T14:41:03.000Z", 26),
            ("12-Nov-2026 07:41:03.118 INFO", "2026-11-12T07:41:03.118Z", 24),
            ("Nov 12 07:41:03 host", "2026-11-12T07:41:03.000Z", 15),
            ("Nov  2 07:41:03.5 host", "2026-11-02T07:41:03.500Z", 17),
            ("Dec 31 23:59:59 host", "2025-12-31T23:59:59.000Z", 15),
            ("Feb 29 10:00:00 x", "2024-02-29T10:00:00.000Z", 15),
            ("1794469263.118 INFO", "2026-11-12T07:41:03.118Z", 14),
            ("1794469263118000 x", "2026-11-12T07:41:03.118Z", 16),
            ("1794469263118000000", "2026-11-12T07:41:03.118Z", 19),
        ];
        for (input, want, n) in cases {
            assert_eq!(scan(input), Some((want.to_string(), *n)), "{input}");
        }
        for bad in ["", "x", "2026-11-12", "2026-11-12x07:00", "123456789", "12345678901abc", "Nov 12", "Foo 12 07:41:03", "99/Nov/2026:07:41:03"] {
            assert_eq!(scan(bad), None, "{bad}");
        }
    }

    #[test]
    fn epoch_numbers() {
        let e = |s: &str| epoch_number(s.as_bytes()).map(rfc);
        assert_eq!(e("1794469263").as_deref(), Some("2026-11-12T07:41:03.000Z"));
        assert_eq!(e("1794469263.5").as_deref(), Some("2026-11-12T07:41:03.500Z"));
        assert_eq!(e("1794469263118").as_deref(), Some("2026-11-12T07:41:03.118Z"));
        assert_eq!(e("1794469263118.9").as_deref(), Some("2026-11-12T07:41:03.118Z"));
        assert_eq!(e("1794469263118123").as_deref(), Some("2026-11-12T07:41:03.118Z"));
        assert_eq!(e("-1"), None);
        assert_eq!(e("1."), None);
        assert_eq!(e("12345678901234567890"), None);
        assert!(e("9223372036854775807").is_some());
        assert_eq!(e("9999999999999999999"), None);
    }

    #[test]
    fn year_of_matches_civil() {
        for y in [1970i64, 1999, 2000, 2024, 2026, 2100] {
            for (m, d) in [(1, 1), (2, 28), (12, 31)] {
                let ts = civil(y, m, d, 12, 0, 0, 0, 0).unwrap();
                assert_eq!(year_of(ts), y);
            }
        }
    }
}
