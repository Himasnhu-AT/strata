//! Log formats: detection from the first 64 KB and parsing one line into a [`Parsed`]. Owned by
//! the `formats` stream (one file per format under `formats/`).
//!
//! Parsing works on bytes (any encoding, invalid UTF-8 included), never panics, cuts every line
//! to [`crate::limits::MAX_LINE_BYTES`] and allocates nothing per line beyond the `fields` vector
//! (reuse one [`Parsed`] with [`Parser::parse_into`] and that goes too). Hostile input is capped:
//! [`MAX_FIELDS_PER_LINE`], [`MAX_FIELD_NAMES`] per file, [`MAX_FIELD_NAME_BYTES`], and JSON is
//! flattened to [`JSON_FLATTEN_DEPTH`] levels (deeper values are kept as raw JSON spans, skipped
//! without recursion).

mod access;
mod json;
mod logfmt;
mod plain;
mod syslog;
mod timestamped;

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{PoisonError, RwLock, RwLockReadGuard};

use serde::{Deserialize, Serialize};

pub use access::AccessParser;
pub use json::JsonParser;
pub use logfmt::LogfmtParser;
pub use plain::PlainParser;
pub use syslog::SyslogParser;
pub use timestamped::TimestampedParser;

use crate::limits::{DETECT_BYTES, MAX_LINE_BYTES};
use crate::time::{epoch_number, scan_timestamp};
use crate::{FieldId, Level, Parsed, Span};

/// Most distinct field names one file interns; later new names are not kept as fields.
pub const MAX_FIELD_NAMES: usize = 4096;
/// Longest field name kept (bytes).
pub const MAX_FIELD_NAME_BYTES: usize = 128;
/// Most fields kept from one line.
pub const MAX_FIELDS_PER_LINE: usize = 256;
/// JSON objects nested this deep are flattened into dotted names (`http.status`); deeper values
/// stay one field holding the raw JSON.
pub const JSON_FLATTEN_DEPTH: usize = 4;
/// Most lines [`detect`] samples.
pub const DETECT_LINES: usize = 256;

/// A file's format. The `as_str` word is what MCP results and the status bar show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FormatKind {
    /// No structure: the whole line is the message.
    #[default]
    Plain,
    /// One JSON object per line.
    JsonLines,
    /// `key=value key2="quoted value"`.
    Logfmt,
    /// RFC 3164 or RFC 5424.
    Syslog,
    /// Text starting with a timestamp (ISO 8601, `[2026-11-12 07:41:03.118]`, …) and usually a
    /// level.
    Timestamped,
    /// Apache / nginx access log (common and combined).
    AccessLog,
}

impl FormatKind {
    /// Every format, in detection order of preference.
    pub const ALL: [FormatKind; 6] =
        [FormatKind::JsonLines, FormatKind::Logfmt, FormatKind::Syslog, FormatKind::AccessLog, FormatKind::Timestamped, FormatKind::Plain];

    /// The id used in MCP results and settings: `plain`, `jsonl`, `logfmt`, `syslog`,
    /// `timestamped`, `access`.
    pub fn as_str(self) -> &'static str {
        match self {
            FormatKind::Plain => "plain",
            FormatKind::JsonLines => "jsonl",
            FormatKind::Logfmt => "logfmt",
            FormatKind::Syslog => "syslog",
            FormatKind::Timestamped => "timestamped",
            FormatKind::AccessLog => "access",
        }
    }

    /// The name shown to people: "Plain text", "JSON lines", …
    pub fn label(self) -> &'static str {
        match self {
            FormatKind::Plain => "Plain text",
            FormatKind::JsonLines => "JSON lines",
            FormatKind::Logfmt => "logfmt",
            FormatKind::Syslog => "syslog",
            FormatKind::Timestamped => "Timestamped text",
            FormatKind::AccessLog => "Access log",
        }
    }

    /// The format with id `s` ([`FormatKind::as_str`]).
    pub fn from_id(s: &str) -> Option<FormatKind> {
        FormatKind::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

/// A detected format and how sure detection is: the share of sampled lines that fit it (for
/// [`FormatKind::Plain`], the share that fit no other format).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    pub kind: FormatKind,
    /// 0.0 to 1.0.
    pub confidence: f32,
}

/// Detect the format from the first bytes of a file (up to [`crate::limits::DETECT_BYTES`]).
/// Never fails: [`FormatKind::Plain`] is the fallback.
pub fn detect(head: &[u8]) -> FormatKind {
    detect_scored(head).kind
}

/// [`detect`] with its confidence. Samples up to [`DETECT_LINES`] lines (blank lines and
/// indented continuation lines such as stack traces are skipped; a last line without its newline
/// is skipped as possibly cut) and picks the format most lines fit, if at least half do;
/// ties go to the earlier format in [`FormatKind::ALL`].
pub fn detect_scored(head: &[u8]) -> Detection {
    let head = head.get(..DETECT_BYTES).unwrap_or(head);
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let complete = head.ends_with(b"\n");
    let names = FieldNames::default();
    let mut scratch = Parsed::default();
    let mut fits = [0u32; FormatKind::ALL.len()];
    let mut n = 0u32;
    let mut lines = head.split(|&c| c == b'\n').peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_none() && !complete && n > 0 {
            break;
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) || matches!(line.first(), Some(b' ' | b'\t')) {
            continue;
        }
        n += 1;
        for (slot, kind) in fits.iter_mut().zip(FormatKind::ALL) {
            if line_fits(kind, line, &names, &mut scratch) {
                *slot += 1;
            }
        }
        if n as usize >= DETECT_LINES {
            break;
        }
    }
    if n == 0 {
        return Detection { kind: FormatKind::Plain, confidence: 0.0 };
    }
    let mut best = (FormatKind::Plain, 0u32);
    for (count, kind) in fits.iter().zip(FormatKind::ALL) {
        if kind != FormatKind::Plain && *count > best.1 {
            best = (kind, *count);
        }
    }
    let share = best.1 as f32 / n as f32;
    if share >= 0.5 { Detection { kind: best.0, confidence: share } } else { Detection { kind: FormatKind::Plain, confidence: 1.0 - share } }
}

/// Whether `line` fits `kind` (what detection counts).
fn line_fits(kind: FormatKind, line: &[u8], names: &FieldNames, out: &mut Parsed) -> bool {
    match kind {
        FormatKind::JsonLines => json::parse_line(line, names, out),
        FormatKind::Logfmt => logfmt::parse_line(line, names, out),
        FormatKind::Syslog => syslog::parse_line(line, names, out),
        FormatKind::AccessLog => access::parse_line(line, names, out),
        FormatKind::Timestamped => timestamped::parse_line(line, names, out),
        FormatKind::Plain => true,
    }
}

/// FNV-1a: field names are short and capped in number, so a simple hash is enough.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}

#[derive(Debug, Default)]
struct Table {
    list: Vec<String>,
    map: HashMap<String, u32, BuildHasherDefault<Fnv>>,
}

/// Field names interned per file, shared between threads (search jobs parse in parallel).
/// Holds at most [`MAX_FIELD_NAMES`] names of at most [`MAX_FIELD_NAME_BYTES`] bytes.
#[derive(Debug, Default)]
pub struct FieldNames {
    names: RwLock<Table>,
}

impl FieldNames {
    /// The id of `name`, adding it when new. When `name` is empty, too long or the table is full
    /// the id is `FieldId(u32::MAX)`, which names nothing (see [`FieldNames::try_intern`]).
    pub fn intern(&self, name: &str) -> FieldId {
        self.try_intern(name).unwrap_or(FieldId(u32::MAX))
    }

    /// The id of `name`, adding it when new; `None` when it cannot be kept (empty, longer than
    /// [`MAX_FIELD_NAME_BYTES`], or [`MAX_FIELD_NAMES`] names already).
    pub fn try_intern(&self, name: &str) -> Option<FieldId> {
        if let Some(id) = self.id(name) {
            return Some(id);
        }
        if name.is_empty() || name.len() > MAX_FIELD_NAME_BYTES {
            return None;
        }
        let mut t = self.names.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&i) = t.map.get(name) {
            return Some(FieldId(i));
        }
        if t.list.len() >= MAX_FIELD_NAMES {
            return None;
        }
        let i = u32::try_from(t.list.len()).ok()?;
        t.list.push(name.to_string());
        t.map.insert(name.to_string(), i);
        Some(FieldId(i))
    }

    /// The id of `name`, if any line had it.
    pub fn id(&self, name: &str) -> Option<FieldId> {
        let t = self.names.read().unwrap_or_else(PoisonError::into_inner);
        t.map.get(name).map(|&i| FieldId(i))
    }

    /// The name of `id`.
    pub fn name(&self, id: FieldId) -> Option<String> {
        let t = self.names.read().unwrap_or_else(PoisonError::into_inner);
        t.list.get(id.0 as usize).cloned()
    }

    /// Every name, in id order.
    pub fn all(&self) -> Vec<String> {
        self.names.read().unwrap_or_else(PoisonError::into_inner).list.clone()
    }
}

/// Interning for one line: holds the read lock across the line's fields and takes the write
/// lock only for a new name.
pub(crate) struct Ids<'a> {
    names: &'a FieldNames,
    guard: Option<RwLockReadGuard<'a, Table>>,
}

impl<'a> Ids<'a> {
    pub(crate) fn new(names: &'a FieldNames) -> Ids<'a> {
        Ids { names, guard: None }
    }

    pub(crate) fn get(&mut self, key: &[u8]) -> Option<FieldId> {
        if key.is_empty() || key.len() > MAX_FIELD_NAME_BYTES {
            return None;
        }
        let key = std::str::from_utf8(key).ok()?;
        let names = self.names;
        let guard = self.guard.get_or_insert_with(|| names.names.read().unwrap_or_else(PoisonError::into_inner));
        if let Some(&i) = guard.map.get(key) {
            return Some(FieldId(i));
        }
        self.guard = None;
        self.names.try_intern(key)
    }
}

/// Splits lines of one format.
pub trait Parser: Send + Sync {
    fn kind(&self) -> FormatKind;

    /// Parse one line (already cut to [`crate::limits::MAX_LINE_BYTES`]). Never fails: a line
    /// that does not fit the format comes back with only `message` set to the whole line.
    fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed;

    /// [`Parser::parse`] into `out`, reusing its `fields` allocation (for loops over many lines).
    fn parse_into(&self, line: &[u8], names: &FieldNames, out: &mut Parsed) {
        *out = self.parse(line, names);
    }

    /// The text of a field value span, with format escapes resolved (JSON `\n`, `\"`, logfmt
    /// quotes). Invalid UTF-8 is replaced.
    fn field_text(&self, line: &[u8], span: Span) -> String {
        String::from_utf8_lossy(span.slice(line).unwrap_or_default()).into_owned()
    }
}

/// The parser for `kind`.
pub fn parser(kind: FormatKind) -> &'static dyn Parser {
    static PLAIN: PlainParser = PlainParser;
    static JSON: JsonParser = JsonParser;
    static LOGFMT: LogfmtParser = LogfmtParser;
    static SYSLOG: SyslogParser = SyslogParser;
    static ACCESS: AccessParser = AccessParser;
    static TIMESTAMPED: TimestampedParser = TimestampedParser;
    match kind {
        FormatKind::Plain => &PLAIN,
        FormatKind::JsonLines => &JSON,
        FormatKind::Logfmt => &LOGFMT,
        FormatKind::Syslog => &SYSLOG,
        FormatKind::AccessLog => &ACCESS,
        FormatKind::Timestamped => &TIMESTAMPED,
    }
}

/// The level a log writes as `word` (bytes, any case): [`Level::parse`]'s words and aliases plus
/// `dbug`, `finer`, `informational`, `emergency`, `crt`. `None` for anything else, numbers
/// included (see [`level_number`]).
pub fn level_word(word: &[u8]) -> Option<Level> {
    if word.is_empty() || word.len() > 13 {
        return None;
    }
    let mut buf = [0u8; 13];
    for (d, s) in buf.iter_mut().zip(word) {
        *d = s.to_ascii_lowercase();
    }
    let w = buf.get(..word.len())?;
    Some(match w {
        b"trace" | b"trc" | b"verbose" | b"vrb" | b"finest" => Level::Trace,
        b"debug" | b"dbg" | b"dbug" | b"fine" | b"finer" => Level::Debug,
        b"info" | b"inf" | b"information" | b"informational" | b"notice" => Level::Info,
        b"warn" | b"warning" | b"wrn" => Level::Warn,
        b"error" | b"err" | b"eror" | b"severe" => Level::Error,
        b"fatal" | b"ftl" | b"crit" | b"crt" | b"critical" | b"panic" | b"emerg" | b"emergency" | b"alert" => Level::Fatal,
        _ => return None,
    })
}

/// A numeric level: syslog severities `0`–`7` (GELF and friends) or bunyan / pino levels
/// (`10` trace, `20` debug, `30` info, `40` warn, `50` error, `60` fatal; in between rounds down).
pub fn level_number(n: i64) -> Option<Level> {
    Some(match n {
        0..=7 => syslog_severity(n),
        10..=19 => Level::Trace,
        20..=29 => Level::Debug,
        30..=39 => Level::Info,
        40..=49 => Level::Warn,
        50..=59 => Level::Error,
        60..=100 => Level::Fatal,
        _ => return None,
    })
}

/// A syslog severity (0 emergency … 7 debug) as a level.
pub(crate) fn syslog_severity(sev: i64) -> Level {
    match sev {
        0..=2 => Level::Fatal,
        3 => Level::Error,
        4 => Level::Warn,
        5 | 6 => Level::Info,
        _ => Level::Debug,
    }
}

pub(crate) fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

/// A span from byte offsets (lines are cut to [`MAX_LINE_BYTES`], so they fit `u32`).
pub(crate) fn sp(start: usize, end: usize) -> Span {
    Span::new(u32::try_from(start).unwrap_or(u32::MAX), u32::try_from(end).unwrap_or(u32::MAX))
}

pub(crate) fn cut(line: &[u8]) -> &[u8] {
    line.get(..MAX_LINE_BYTES).unwrap_or(line)
}

/// Clear `out` for a new line, keeping the fields allocation.
pub(crate) fn reset(out: &mut Parsed) {
    out.time = None;
    out.level = None;
    out.fields.clear();
    out.message = Span::default();
}

/// `out` as a line that does not fit: only the message, the whole line.
pub(crate) fn whole(out: &mut Parsed, line: &[u8]) {
    reset(out);
    out.message = sp(0, line.len());
}

pub(crate) fn push_field(out: &mut Parsed, ids: &mut Ids<'_>, key: &[u8], value: Span) {
    if out.fields.len() >= MAX_FIELDS_PER_LINE {
        return;
    }
    if let Some(id) = ids.get(key) {
        out.fields.push((id, value));
    }
}

/// What a key means in JSON and logfmt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Time,
    Level,
    Message,
    Other,
}

fn role(key: &[u8]) -> Role {
    if key.len() > 14 {
        return Role::Other;
    }
    let mut buf = [0u8; 14];
    for (d, s) in buf.iter_mut().zip(key) {
        *d = s.to_ascii_lowercase();
    }
    match buf.get(..key.len()).unwrap_or_default() {
        b"ts" | b"time" | b"timestamp" | b"@timestamp" | b"t" | b"@t" | b"date" | b"datetime" | b"eventtime" | b"time_local" => Role::Time,
        b"level" | b"lvl" | b"severity" | b"loglevel" | b"log_level" | b"log.level" | b"levelname" | b"level_name" | b"@l" | b"@level"
        | b"severity_text" => Role::Level,
        b"msg" | b"message" | b"@message" | b"@m" | b"log" => Role::Message,
        _ => Role::Other,
    }
}

/// A value found next to a key: quoted text (span without quotes), a bare word or number, or
/// raw JSON (an object or array).
#[derive(Clone, Copy)]
pub(crate) enum Value {
    Quoted(usize, usize),
    Bare(usize, usize),
    Raw(usize, usize),
}

/// Record `key = value` in `out`: the first time-, level- and message-like keys whose values
/// read as such fill `time`, `level` and `message`; everything else is a field.
pub(crate) fn emit(line: &[u8], ids: &mut Ids<'_>, out: &mut Parsed, has_msg: &mut bool, key: &[u8], value: Value) {
    let (s, e) = match value {
        Value::Quoted(s, e) | Value::Bare(s, e) | Value::Raw(s, e) => (s, e),
    };
    let text = line.get(s..e).unwrap_or_default();
    match role(key) {
        Role::Time if out.time.is_none() => {
            let t = match value {
                Value::Quoted(..) => scan_timestamp(text).map(|(t, _)| t).or_else(|| epoch_number(text)),
                Value::Bare(..) => epoch_number(text).or_else(|| scan_timestamp(text).map(|(t, _)| t)),
                Value::Raw(..) => None,
            };
            if t.is_some() {
                out.time = t;
                return;
            }
        }
        Role::Level if out.level.is_none() => {
            let l = match value {
                Value::Raw(..) => None,
                _ => level_word(text).or_else(|| std::str::from_utf8(text).ok().and_then(|t| t.parse::<i64>().ok()).and_then(level_number)),
            };
            if l.is_some() {
                out.level = l;
                return;
            }
        }
        Role::Message if !*has_msg && !matches!(value, Value::Raw(..)) => {
            out.message = sp(s, e);
            *has_msg = true;
            return;
        }
        _ => {}
    }
    push_field(out, ids, key, sp(s, e));
}

/// The end of a `"…"` string starting at `i` (which holds the opening quote), honouring
/// backslash escapes: (content start, content end, has escapes, index after the closing quote).
/// `None` when it never closes.
pub(crate) fn quoted(line: &[u8], i: usize) -> Option<(usize, usize, bool, usize)> {
    let s = i + 1;
    let mut j = s;
    let mut esc = false;
    loop {
        let rel = memchr::memchr2(b'"', b'\\', line.get(j..)?)?;
        j += rel;
        if at(line, j) == b'\\' {
            esc = true;
            j += 2;
        } else {
            return Some((s, j, esc, j + 1));
        }
    }
}

/// `key=value` tokens in free text (a syslog or timestamped message) between `start` and `end`:
/// keys `[A-Za-z_][A-Za-z0-9_.-]*` after a space or the start, values bare or `"quoted"`.
pub(crate) fn scan_kv(line: &[u8], start: usize, end: usize, ids: &mut Ids<'_>, out: &mut Parsed) {
    let end = end.min(line.len());
    let mut i = start;
    while i < end {
        while i < end && matches!(at(line, i), b' ' | b'\t') {
            i += 1;
        }
        let ks = i;
        if !(at(line, i).is_ascii_alphabetic() || at(line, i) == b'_') {
            i = next_space(line, i, end);
            continue;
        }
        while i < end && (at(line, i).is_ascii_alphanumeric() || matches!(at(line, i), b'_' | b'.' | b'-')) {
            i += 1;
        }
        let ke = i;
        if at(line, i) != b'=' || i >= end || ke - ks > 64 {
            i = next_space(line, i, end);
            continue;
        }
        i += 1;
        if at(line, i) == b'"' {
            match quoted(line, i) {
                Some((s, e, _, next)) if next <= end => {
                    push_field(out, ids, line.get(ks..ke).unwrap_or_default(), sp(s, e));
                    i = next;
                }
                _ => i = next_space(line, i, end),
            }
            continue;
        }
        let vs = i;
        i = next_space(line, i, end);
        // Trailing punctuation of prose (`id=42,` `(ms=3)`) is not part of the value.
        let mut ve = i;
        while ve > vs && matches!(at(line, ve - 1), b',' | b';' | b')' | b'.') {
            ve -= 1;
        }
        if ve > vs {
            push_field(out, ids, line.get(ks..ke).unwrap_or_default(), sp(vs, ve));
        }
    }
}

pub(crate) fn next_space(line: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && !matches!(at(line, i), b' ' | b'\t') {
        i += 1;
    }
    i
}

/// `raw` with backslash escapes resolved (JSON's `\n \t \r \b \f \/ \\ \" \uXXXX`, surrogate
/// pairs included); anything else after a backslash is kept as written. Invalid UTF-8 and
/// lone surrogates become U+FFFD.
pub(crate) fn unescape(raw: &[u8]) -> String {
    if !raw.contains(&b'\\') {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = at(raw, i);
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let simple = match at(raw, i + 1) {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'/' => Some(b'/'),
            b'\\' => Some(b'\\'),
            b'"' => Some(b'"'),
            _ => None,
        };
        if let Some(s) = simple {
            out.push(s);
            i += 2;
            continue;
        }
        if at(raw, i + 1) == b'u'
            && let Some(hi) = hex4(raw, i + 2)
        {
            let (ch, used) = if (0xD800..0xDC00).contains(&hi) && at(raw, i + 6) == b'\\' && at(raw, i + 7) == b'u' {
                match hex4(raw, i + 8) {
                    Some(lo) if (0xDC00..0xE000).contains(&lo) => (char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)), 12),
                    _ => (None, 6),
                }
            } else {
                (char::from_u32(hi), 6)
            };
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.unwrap_or('\u{FFFD}').encode_utf8(&mut buf).as_bytes());
            i += used;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex4(b: &[u8], i: usize) -> Option<u32> {
    let mut v = 0u32;
    for k in 0..4 {
        v = v * 16 + char::from(at(b, i + k)).to_digit(16)?;
    }
    Some(v)
}

/// A word with surrounding brackets, parentheses, angle brackets and a trailing colon removed
/// (`[INFO]`, `<Error>`, `WARN:`).
pub(crate) fn strip_word(w: &[u8]) -> &[u8] {
    let mut w = w;
    while let Some(rest) = w.strip_suffix(b":").or_else(|| w.strip_suffix(b",")) {
        w = rest;
    }
    if let (Some(&f), Some(&l)) = (w.first(), w.last())
        && w.len() >= 2
        && matches!((f, l), (b'[', b']') | (b'(', b')') | (b'<', b'>') | (b'{', b'}'))
    {
        return w.get(1..w.len() - 1).unwrap_or_default();
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_names_intern() {
        for f in FormatKind::ALL {
            assert_eq!(FormatKind::from_id(f.as_str()), Some(f));
        }
        let n = FieldNames::default();
        let a = n.intern("service");
        assert_eq!(n.intern("service"), a);
        assert_eq!(n.name(a).as_deref(), Some("service"));
        assert_eq!(n.try_intern(""), None);
        assert_eq!(n.try_intern(&"x".repeat(MAX_FIELD_NAME_BYTES + 1)), None);
        assert_eq!(n.intern(""), FieldId(u32::MAX));
    }

    #[test]
    fn field_names_are_capped() {
        let n = FieldNames::default();
        for i in 0..MAX_FIELD_NAMES + 10 {
            let _ = n.intern(&format!("f{i}"));
        }
        assert_eq!(n.all().len(), MAX_FIELD_NAMES);
        assert_eq!(n.try_intern("one-more"), None);
        assert_eq!(n.try_intern("f0"), Some(FieldId(0)));
    }

    #[test]
    fn levels_normalise() {
        let cases: &[(&str, Option<Level>)] = &[
            ("TRACE", Some(Level::Trace)),
            ("dbg", Some(Level::Debug)),
            ("Info", Some(Level::Info)),
            ("notice", Some(Level::Info)),
            ("WARNING", Some(Level::Warn)),
            ("wrn", Some(Level::Warn)),
            ("ERR", Some(Level::Error)),
            ("severe", Some(Level::Error)),
            ("CRIT", Some(Level::Fatal)),
            ("emergency", Some(Level::Fatal)),
            ("panic", Some(Level::Fatal)),
            ("informational", Some(Level::Info)),
            ("information!", None),
            ("", None),
            ("30", None),
        ];
        for (w, want) in cases {
            assert_eq!(level_word(w.as_bytes()), *want, "{w}");
        }
        assert_eq!(level_number(30), Some(Level::Info));
        assert_eq!(level_number(50), Some(Level::Error));
        assert_eq!(level_number(60), Some(Level::Fatal));
        assert_eq!(level_number(10), Some(Level::Trace));
        assert_eq!(level_number(3), Some(Level::Error));
        assert_eq!(level_number(6), Some(Level::Info));
        assert_eq!(level_number(8), None);
        assert_eq!(level_number(-1), None);
        // Every contract word round-trips.
        for l in Level::ALL {
            assert_eq!(level_word(l.as_str().as_bytes()), Some(l));
        }
    }

    #[test]
    fn unescape_resolves_json_escapes() {
        assert_eq!(unescape(br"a\nb\t\x22\u00e9\ud83d\ude00\\"), "a\nb\t\\x22é😀\\");
        assert_eq!(unescape(br#"say \"hi\""#), r#"say "hi""#);
        assert_eq!(unescape(br"\ud83d alone"), "\u{FFFD} alone");
        assert_eq!(unescape(b"trailing\\"), "trailing\\");
        assert_eq!(unescape(br"\u12"), "\\u12");
    }

    #[test]
    fn detects_each_format() {
        let cases: &[(&str, FormatKind)] = &[
            ("{\"ts\":\"2026-11-12T07:41:03Z\",\"level\":\"info\",\"msg\":\"hi\"}\n{\"a\":1}\n", FormatKind::JsonLines),
            ("time=2026-11-12T07:41:03Z level=info msg=hi\nlevel=warn msg=\"x y\" a=1\n", FormatKind::Logfmt),
            ("<14>1 2026-11-12T07:41:03Z host app 12 - - hi\n<11>1 2026-11-12T07:41:04Z host app 12 - - err\n", FormatKind::Syslog),
            ("Nov 12 07:41:03 web sshd[123]: accepted\nNov 12 07:41:04 web cron[9]: ran\n", FormatKind::Syslog),
            ("1.2.3.4 - - [12/Nov/2026:07:41:03 +0000] \"GET / HTTP/1.1\" 200 12 \"-\" \"curl\"\n", FormatKind::AccessLog),
            ("[2026-11-12 07:41:03.118] INFO api: hi\n[2026-11-12 07:41:04.000] WARN api: slow\n  at x\n", FormatKind::Timestamped),
            ("2026-11-12 07:41:03,118 ERROR boom\n", FormatKind::Timestamped),
            ("hello\nworld\n", FormatKind::Plain),
            ("", FormatKind::Plain),
        ];
        for (head, want) in cases {
            assert_eq!(detect(head.as_bytes()), *want, "{head}");
        }
        let d = detect_scored(b"{\"a\":1}\nnot json\n{\"b\":2}\n{\"c\":3}\n");
        assert_eq!(d.kind, FormatKind::JsonLines);
        assert!((d.confidence - 0.75).abs() < 1e-6);
        // A cut last line does not count.
        assert_eq!(detect_scored(b"{\"a\":1}\n{\"a\":2}\n{\"tru").confidence, 1.0);
    }

    #[test]
    fn parser_kinds_match() {
        for f in FormatKind::ALL {
            assert_eq!(parser(f).kind(), f);
        }
    }

    #[test]
    fn kv_in_free_text() {
        let names = FieldNames::default();
        let mut ids = Ids::new(&names);
        let mut out = Parsed::default();
        let line = b"done id=42, user=\"a b\" =x 9=1 path=/v1 ratio=(3) tail=";
        scan_kv(line, 0, line.len(), &mut ids, &mut out);
        drop(ids);
        let got: Vec<(String, String)> =
            out.fields.iter().map(|(f, s)| (names.name(*f).unwrap(), String::from_utf8_lossy(s.slice(line).unwrap()).into_owned())).collect();
        let want = [("id", "42"), ("user", "a b"), ("path", "/v1"), ("ratio", "(3")];
        assert_eq!(got, want.map(|(a, b)| (a.to_string(), b.to_string())));
    }
}
