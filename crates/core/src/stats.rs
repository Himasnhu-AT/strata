//! File statistics and field summaries: `log.stats`, `log.fields`, the column chooser. Owned by
//! the `search` stream.

use serde::{Deserialize, Serialize};

use crate::{LevelCounts, LogFile, Result, Timestamp};

/// The type a field's values mostly have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    #[default]
    String,
    Number,
    Bool,
    Time,
    /// A JSON object or array.
    Object,
    /// Values of several types.
    Mixed,
}

impl FieldType {
    /// `string`, `number`, `bool`, `time`, `object`, `mixed`.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Number => "number",
            FieldType::Bool => "bool",
            FieldType::Time => "time",
            FieldType::Object => "object",
            FieldType::Mixed => "mixed",
        }
    }
}

/// One parsed field over the lines looked at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldSummary {
    pub name: String,
    pub ty: FieldType,
    /// Lines that have it.
    pub lines: u64,
    /// The most common values with their counts, most common first (capped by the caller).
    pub top: Vec<(String, u64)>,
    /// Distinct values seen (a lower bound once the sketch overflows).
    pub distinct: u64,
}

/// What `log.stats` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileStats {
    pub lines: u64,
    pub bytes: u64,
    pub first_time: Option<Timestamp>,
    pub last_time: Option<Timestamp>,
    pub levels: LevelCounts,
    /// Most frequent fields, most lines first.
    pub fields: Vec<FieldSummary>,
    /// Whether every line was looked at (`false`: a sample, for files still indexing or huge).
    pub exact: bool,
}

/// Statistics of `file`, optionally for lines whose time is in `from..to`. Fields are summarised
/// with at most `top_fields` fields and `top_values` values each.
pub fn stats(
    file: &LogFile,
    from: Option<Timestamp>,
    to: Option<Timestamp>,
    top_fields: usize,
    top_values: usize,
    cancel: &dyn Fn() -> bool,
) -> Result<FileStats> {
    let _ = (from, to, top_fields, top_values, cancel);
    Ok(FileStats { lines: file.lines(), bytes: file.len(), ..FileStats::default() })
}

/// Every parsed field of `file` with its type and `top_values` most common values.
pub fn fields(file: &LogFile, top_values: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<FieldSummary>> {
    let _ = (file, top_values, cancel);
    Ok(Vec::new())
}
