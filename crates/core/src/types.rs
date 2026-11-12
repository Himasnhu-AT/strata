//! Small shared types: ids, levels, spans, timestamps and a parsed line. Owned by the lead.

use serde::{Deserialize, Serialize};

/// A 0-based line number in a file.
pub type LineNo = u64;

/// An open file, unique for the life of the workspace (never reused after a close). Shown to
/// agents as a plain integer (`"file": 1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub u64);

/// A field name interned per file by [`crate::FieldNames`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldId(pub u32);

/// A byte range inside one line (`start..end`, `start <= end`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// A span from `start` to `end`; swapped when given backwards.
    pub fn new(start: u32, end: u32) -> Span {
        if start <= end { Span { start, end } } else { Span { start: end, end: start } }
    }

    /// Length in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span is empty.
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// The bytes of `line` this span covers; `None` when it is out of bounds.
    pub fn slice(self, line: &[u8]) -> Option<&[u8]> {
        line.get(self.start as usize..self.end as usize)
    }
}

/// A point in time: microseconds since the Unix epoch, UTC. Parsed from the log by
/// [`crate::formats`]; shown to people and agents as RFC 3339 ([`Timestamp::to_rfc3339`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub const MICROS_PER_SECOND: i64 = 1_000_000;

    /// From whole seconds since the epoch (saturating).
    pub fn from_secs(secs: i64) -> Timestamp {
        Timestamp(secs.saturating_mul(Self::MICROS_PER_SECOND))
    }

    /// From milliseconds since the epoch (saturating).
    pub fn from_millis(ms: i64) -> Timestamp {
        Timestamp(ms.saturating_mul(1000))
    }

    /// `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC, milliseconds): the form every MCP result uses.
    pub fn to_rfc3339(self) -> String {
        let micros = self.0;
        let secs = micros.div_euclid(Self::MICROS_PER_SECOND);
        let ms = micros.rem_euclid(Self::MICROS_PER_SECOND) / 1000;
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", sod / 3600, sod % 3600 / 60, sod % 60)
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A log level, ordered from least to most severe. Shown as its lowercase word
/// ([`Level::as_str`]): never by colour alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// Every level, least severe first.
    pub const ALL: [Level; 6] = [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error, Level::Fatal];

    /// The word used in the UI, the filter grammar and MCP results: `trace`, `debug`, `info`,
    /// `warn`, `error`, `fatal`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    /// The level a log writes as `word`, ignoring case: the words of [`Level::as_str`] plus the
    /// usual aliases (`warning`, `err`, `crit`, `critical`, `panic`, `emerg`, `alert`, `notice`
    /// as info, `verbose`/`trc`/`dbg`/`inf`/`wrn`/`ftl`, syslog severities by name).
    pub fn parse(word: &str) -> Option<Level> {
        let w = word.trim();
        let eq = |s: &str| w.eq_ignore_ascii_case(s);
        Some(if eq("trace") || eq("trc") || eq("verbose") || eq("finest") {
            Level::Trace
        } else if eq("debug") || eq("dbg") || eq("fine") {
            Level::Debug
        } else if eq("info") || eq("inf") || eq("information") || eq("notice") {
            Level::Info
        } else if eq("warn") || eq("warning") || eq("wrn") {
            Level::Warn
        } else if eq("error") || eq("err") || eq("eror") || eq("severe") {
            Level::Error
        } else if eq("fatal") || eq("ftl") || eq("crit") || eq("critical") || eq("panic") || eq("emerg") || eq("alert") {
            Level::Fatal
        } else {
            return None;
        })
    }

    /// Position in [`Level::ALL`] (0..6).
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Line counts per level, plus lines without a level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LevelCounts {
    /// Indexed by [`Level::index`].
    pub by_level: [u64; 6],
    /// Lines with no recognised level.
    pub none: u64,
}

impl LevelCounts {
    /// Count one line of `level`.
    pub fn add(&mut self, level: Option<Level>) {
        match level {
            Some(l) => {
                if let Some(c) = self.by_level.get_mut(l.index()) {
                    *c = c.saturating_add(1);
                }
            }
            None => self.none = self.none.saturating_add(1),
        }
    }

    /// Lines of `level`.
    pub fn get(&self, level: Level) -> u64 {
        self.by_level.get(level.index()).copied().unwrap_or(0)
    }

    /// Every line counted.
    pub fn total(&self) -> u64 {
        self.by_level.iter().fold(self.none, |a, b| a.saturating_add(*b))
    }

    /// Add `other` into `self`.
    pub fn merge(&mut self, other: &LevelCounts) {
        for (a, b) in self.by_level.iter_mut().zip(other.by_level) {
            *a = a.saturating_add(b);
        }
        self.none = self.none.saturating_add(other.none);
    }
}

/// One line split by its format: parsed lazily for visible lines and cached by line number.
/// Spans point into the line's bytes as read (after the [`crate::limits::MAX_LINE_BYTES`] cut).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub time: Option<Timestamp>,
    pub level: Option<Level>,
    /// Other fields in the order they appear, values as spans (JSON strings without quotes;
    /// escapes are resolved by [`crate::formats::Parser::field_text`]).
    pub fields: Vec<(FieldId, Span)>,
    /// The message: the `msg`/`message` field, or the text after time and level.
    pub message: Span,
}

impl Parsed {
    /// The value span of field `id`, first occurrence.
    pub fn field(&self, id: FieldId) -> Option<Span> {
        self.fields.iter().find(|(f, _)| *f == id).map(|(_, s)| *s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_utc_millis() {
        assert_eq!(Timestamp(0).to_rfc3339(), "1970-01-01T00:00:00.000Z");
        assert_eq!(Timestamp(1_794_469_263_118_000).to_rfc3339(), "2026-11-12T07:41:03.118Z");
        assert_eq!(Timestamp(-1000).to_rfc3339(), "1969-12-31T23:59:59.999Z");
        // never panics at the extremes
        let _ = Timestamp(i64::MIN).to_rfc3339();
        let _ = Timestamp(i64::MAX).to_rfc3339();
    }

    #[test]
    fn levels_parse_and_count() {
        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("nope"), None);
        for l in Level::ALL {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        let mut c = LevelCounts::default();
        c.add(Some(Level::Error));
        c.add(None);
        assert_eq!((c.get(Level::Error), c.total()), (1, 2));
    }
}
