//! Log formats: detection from the first 64 KB and parsing one line into a [`Parsed`]. Owned by
//! the `formats` stream (one file per format under `formats/`).

use std::sync::{PoisonError, RwLock};

use serde::{Deserialize, Serialize};

use crate::{FieldId, Parsed, Span};

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

/// Detect the format from the first bytes of a file (up to [`crate::limits::DETECT_BYTES`]).
/// Never fails: [`FormatKind::Plain`] is the fallback.
pub fn detect(head: &[u8]) -> FormatKind {
    let _ = head;
    FormatKind::Plain
}

/// Field names interned per file, shared between threads (search jobs parse in parallel).
#[derive(Debug, Default)]
pub struct FieldNames {
    names: RwLock<Vec<String>>,
}

impl FieldNames {
    /// The id of `name`, adding it when new.
    pub fn intern(&self, name: &str) -> FieldId {
        if let Some(id) = self.id(name) {
            return id;
        }
        let mut names = self.names.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = names.iter().position(|n| n == name) {
            return FieldId(u32::try_from(i).unwrap_or(u32::MAX));
        }
        names.push(name.to_string());
        FieldId(u32::try_from(names.len().saturating_sub(1)).unwrap_or(u32::MAX))
    }

    /// The id of `name`, if any line had it.
    pub fn id(&self, name: &str) -> Option<FieldId> {
        let names = self.names.read().unwrap_or_else(PoisonError::into_inner);
        names.iter().position(|n| n == name).and_then(|i| u32::try_from(i).ok()).map(FieldId)
    }

    /// The name of `id`.
    pub fn name(&self, id: FieldId) -> Option<String> {
        let names = self.names.read().unwrap_or_else(PoisonError::into_inner);
        names.get(id.0 as usize).cloned()
    }

    /// Every name, in id order.
    pub fn all(&self) -> Vec<String> {
        self.names.read().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// Splits lines of one format.
pub trait Parser: Send + Sync {
    fn kind(&self) -> FormatKind;

    /// Parse one line (already cut to [`crate::limits::MAX_LINE_BYTES`]). Never fails: a line
    /// that does not fit the format comes back with only `message` set to the whole line.
    fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed;

    /// The text of a field value span, with format escapes resolved (JSON `\n`, `\"`, logfmt
    /// quotes). Invalid UTF-8 is replaced.
    fn field_text(&self, line: &[u8], span: Span) -> String {
        String::from_utf8_lossy(span.slice(line).unwrap_or_default()).into_owned()
    }
}

/// The parser for `kind`.
pub fn parser(kind: FormatKind) -> &'static dyn Parser {
    static PLAIN: PlainParser = PlainParser;
    let _ = kind;
    &PLAIN
}

/// The fallback: the whole line is the message.
#[derive(Debug, Default)]
pub struct PlainParser;

impl Parser for PlainParser {
    fn kind(&self) -> FormatKind {
        FormatKind::Plain
    }

    fn parse(&self, line: &[u8], _names: &FieldNames) -> Parsed {
        let end = u32::try_from(line.len()).unwrap_or(u32::MAX);
        Parsed { message: Span::new(0, end), ..Parsed::default() }
    }
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
    }
}
