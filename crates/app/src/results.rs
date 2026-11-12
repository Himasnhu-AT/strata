//! The JSON results of the agent (MCP) commands: one type per command, frozen. Owned by the lead.
//!
//! Every handler in `commands/agent.rs` returns `serde_json::to_value(<Result type>)`. Each type
//! has an `example()` whose JSON is the golden file `crates/app/tests/mcp/<command>.json`
//! (`tests/mcp_shapes.rs` checks the shapes match). Changing a type is a contract change: add
//! fields only, update `example()`, regenerate the golden files with `STRATA_UPDATE_GOLDEN=1
//! cargo test -p strata --test mcp_shapes`, and tell the streams.
//!
//! Conventions: line numbers are 1-based; times are RFC 3339 UTC strings with milliseconds
//! (`2026-11-12T07:41:03.118Z`) or `null` when the line has none; levels are the words of
//! `Level::as_str` or `null`; every list is capped and says how many it left out (`omitted`);
//! line text is redacted and cut to 2 KB (`cut: true`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::ids;

/// The error text when the person turned off "Allow the agent to change the view".
pub const DENIED: &str = "the person turned off agent changes (Settings ▸ AI ▸ Allow the agent to change the view)";

/// Where indexing of a file is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Indexing {
    /// `idle`, `running`, `complete`, `cancelled` or `failed`.
    pub state: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Why it failed (`state = failed`).
    pub error: Option<String>,
}

/// A time range, both ends RFC 3339.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimeSpan {
    pub from: String,
    pub to: String,
}

/// Line counts per level (`none`: lines without a level).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub trace: u64,
    pub debug: u64,
    pub info: u64,
    pub warn: u64,
    pub error: u64,
    pub fatal: u64,
    pub none: u64,
}

/// A value and how many lines have it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
}

/// One parsed field of a line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldValue {
    pub name: String,
    /// Redacted, cut to 2 KB.
    pub value: String,
}

/// A line in a search result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub line: u64,
    pub time: Option<String>,
    pub level: Option<String>,
    /// The whole line, redacted, cut to 2 KB.
    pub text: String,
    pub cut: bool,
}

/// A line with its parsed fields (`log.lines`, `log.around`, `log.first`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LineOut {
    pub line: u64,
    pub time: Option<String>,
    pub level: Option<String>,
    /// The message part (redacted, cut).
    pub message: String,
    /// The whole line (redacted, cut).
    pub text: String,
    pub cut: bool,
    /// Parsed fields other than time, level and message, in line order.
    pub fields: Vec<FieldValue>,
}

/// `log.open`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Open {
    pub file: u64,
    pub path: String,
    pub bytes: u64,
    /// `plain`, `jsonl`, `logfmt`, `syslog`, `timestamped`, `access`.
    pub format: String,
    /// Lines indexed so far.
    pub lines: u64,
    pub following: bool,
    pub indexing: Indexing,
}

/// One entry of `log.files`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub file: u64,
    pub path: String,
    pub format: String,
    pub bytes: u64,
    pub lines: u64,
    pub time_span: Option<TimeSpan>,
    pub following: bool,
    /// The file shown in the window.
    pub active: bool,
    pub indexing: Indexing,
}

/// `log.files`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Files {
    pub files: Vec<FileInfo>,
    pub omitted: u64,
}

/// A field in `log.stats`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopField {
    pub name: String,
    pub lines: u64,
    pub values: Vec<ValueCount>,
}

/// `log.stats`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub file: u64,
    pub lines: u64,
    pub bytes: u64,
    pub time_span: Option<TimeSpan>,
    pub levels: Levels,
    pub top_fields: Vec<TopField>,
    /// `false`: computed from a sample (the file is still indexing, or huge).
    pub exact: bool,
}

/// `log.search`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Search {
    pub file: u64,
    pub matches: Vec<Hit>,
    /// Every matching line in the searched part.
    pub total: u64,
    /// `total` minus the matches returned.
    pub omitted: u64,
    /// `false`: the file is still indexing, so more matches may come.
    pub complete: bool,
}

/// `log.lines`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lines {
    pub file: u64,
    pub lines: Vec<LineOut>,
    /// Lines indexed in the file so far.
    pub total_lines: u64,
    /// Requested lines not returned (past the end or over the cap).
    pub omitted: u64,
}

/// `log.around`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Around {
    pub file: u64,
    /// The line asked about (it is in `lines`).
    pub line: u64,
    pub lines: Vec<LineOut>,
}

/// One bucket of `log.histogram`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BucketOut {
    pub start: String,
    pub levels: Levels,
}

/// `log.histogram`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistogramOut {
    pub file: u64,
    /// The bucket width as a duration (`1s`, `5m`, `1h`).
    pub bucket: String,
    pub buckets: Vec<BucketOut>,
    /// Matching lines without a timestamp.
    pub untimed: u64,
    pub omitted: u64,
}

/// A field in `log.fields`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldOut {
    pub name: String,
    /// `string`, `number`, `bool`, `time`, `object`, `mixed`.
    #[serde(rename = "type")]
    pub ty: String,
    pub lines: u64,
    pub distinct: u64,
    /// The 20 most common values, most common first (redacted, cut).
    pub values: Vec<ValueCount>,
}

/// `log.fields`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fields {
    pub file: u64,
    pub fields: Vec<FieldOut>,
    pub omitted: u64,
}

/// `log.first`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct First {
    pub file: u64,
    /// The canonical form of the filter that was used.
    pub filter: String,
    /// `null` when no indexed line matches.
    pub line: Option<LineOut>,
    /// `false`: the file is still indexing, so a match may come later.
    pub complete: bool,
}

/// `view.filter`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filtered {
    pub file: u64,
    /// The id of the filter chip that was added.
    pub filter_id: u64,
    /// The canonical form of the filter.
    pub filter: String,
    /// Lines visible with every active filter.
    pub visible: u64,
    /// `false`: still counting (the file is indexing).
    pub complete: bool,
}

/// `view.clear_filters`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cleared {
    pub file: u64,
    /// Filter chips removed.
    pub removed: u64,
}

/// `view.goto`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Goto {
    pub file: u64,
    /// The line scrolled to (for `time`: the first line at or after it).
    pub line: u64,
}

/// `view.highlight`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Highlighted {
    pub file: u64,
    pub highlight: u64,
    /// Lines highlighted.
    pub lines: u64,
    /// Lines left out over the cap of 500.
    pub omitted: u64,
}

/// `bookmark.add`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BookmarkAdded {
    pub file: u64,
    pub bookmark: u64,
    pub line: u64,
    pub name: String,
}

/// One entry of `bookmark.list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BookmarkOut {
    pub bookmark: u64,
    pub line: u64,
    pub name: String,
    pub note: Option<String>,
    /// `amber`, `red`, `yellow`, `blue`, `green`, `grey`.
    pub color: String,
    /// `user` or `agent`.
    pub by: String,
}

/// `bookmark.list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bookmarks {
    pub file: u64,
    pub bookmarks: Vec<BookmarkOut>,
    pub omitted: u64,
}

/// `agent.note`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Noted {
    pub note: u64,
}

/// `agent.undo`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Undone {
    /// Agent changes (filters, highlights, bookmarks, notes) removed.
    pub undone: u64,
}

// ---------------------------------------------------------------------------------------------
// Examples: the golden files are these, serialized.
// ---------------------------------------------------------------------------------------------

const T0: &str = "2026-11-12T14:02:07.118Z";
const T1: &str = "2026-11-12T15:30:00.000Z";
const PATH: &str = "/var/log/payments/app.log";
const ERR_LINE: &str = "2026-11-12 14:02:07.118 ERROR [payments] upstream timeout request_id=r-4821 status=502";

fn indexing() -> Indexing {
    Indexing { state: "running".into(), done_bytes: 52_428_800, total_bytes: 104_857_600, error: None }
}

fn levels() -> Levels {
    Levels { trace: 0, debug: 120_400, info: 1_120_013, warn: 38_200, error: 5_420, fatal: 0, none: 80 }
}

fn span() -> Option<TimeSpan> {
    Some(TimeSpan { from: "2026-11-12T00:00:00.004Z".into(), to: T1.into() })
}

fn vc(value: &str, count: u64) -> ValueCount {
    ValueCount { value: value.into(), count }
}

fn line_out(line: u64) -> LineOut {
    LineOut {
        line,
        time: Some(T0.into()),
        level: Some("error".into()),
        message: "upstream timeout request_id=r-4821 status=502".into(),
        text: ERR_LINE.into(),
        cut: false,
        fields: vec![
            FieldValue { name: "service".into(), value: "payments".into() },
            FieldValue { name: "request_id".into(), value: "r-4821".into() },
            FieldValue { name: "status".into(), value: "502".into() },
        ],
    }
}

impl Open {
    pub fn example() -> Open {
        Open { file: 1, path: PATH.into(), bytes: 104_857_600, format: "timestamped".into(), lines: 412_000, following: false, indexing: indexing() }
    }
}

impl Files {
    pub fn example() -> Files {
        let f = FileInfo {
            file: 1,
            path: PATH.into(),
            format: "timestamped".into(),
            bytes: 104_857_600,
            lines: 1_284_113,
            time_span: span(),
            following: true,
            active: true,
            indexing: Indexing { state: "complete".into(), done_bytes: 104_857_600, total_bytes: 104_857_600, error: None },
        };
        let failed = FileInfo {
            file: 2,
            path: "/var/log/secure.log".into(),
            format: "plain".into(),
            bytes: 0,
            lines: 0,
            time_span: None,
            following: false,
            active: false,
            indexing: Indexing {
                state: "failed".into(),
                done_bytes: 0,
                total_bytes: 0,
                error: Some("/var/log/secure.log: permission denied".into()),
            },
        };
        Files { files: vec![f, failed], omitted: 0 }
    }
}

impl Stats {
    pub fn example() -> Stats {
        Stats {
            file: 1,
            lines: 1_284_113,
            bytes: 104_857_600,
            time_span: span(),
            levels: levels(),
            top_fields: vec![TopField { name: "service".into(), lines: 1_284_000, values: vec![vc("api", 700_000), vc("payments", 400_000)] }],
            exact: true,
        }
    }
}

impl Search {
    pub fn example() -> Search {
        let hit = Hit { line: 48_213, time: Some(T0.into()), level: Some("error".into()), text: ERR_LINE.into(), cut: false };
        Search { file: 1, matches: vec![hit], total: 312, omitted: 311, complete: true }
    }
}

impl Lines {
    pub fn example() -> Lines {
        Lines { file: 1, lines: vec![line_out(48_213)], total_lines: 1_284_113, omitted: 0 }
    }
}

impl Around {
    pub fn example() -> Around {
        Around { file: 1, line: 48_213, lines: vec![line_out(48_212), line_out(48_213), line_out(48_214)] }
    }
}

impl HistogramOut {
    pub fn example() -> HistogramOut {
        let b = BucketOut { start: "2026-11-12T14:00:00.000Z".into(), levels: levels() };
        HistogramOut { file: 1, bucket: "1m".into(), buckets: vec![b], untimed: 80, omitted: 0 }
    }
}

impl Fields {
    pub fn example() -> Fields {
        let f = FieldOut {
            name: "status".into(),
            ty: "number".into(),
            lines: 1_200_000,
            distinct: 14,
            values: vec![vc("200", 1_100_000), vc("502", 5_100)],
        };
        Fields { file: 1, fields: vec![f], omitted: 0 }
    }
}

impl First {
    pub fn example() -> First {
        First { file: 1, filter: "level >= error and time >= \"2026-11-12T14:00:00.000Z\"".into(), line: Some(line_out(48_213)), complete: true }
    }
}

impl Filtered {
    pub fn example() -> Filtered {
        Filtered { file: 1, filter_id: 7, filter: "service = payments".into(), visible: 401_120, complete: true }
    }
}

impl Cleared {
    pub fn example() -> Cleared {
        Cleared { file: 1, removed: 2 }
    }
}

impl Goto {
    pub fn example() -> Goto {
        Goto { file: 1, line: 48_213 }
    }
}

impl Highlighted {
    pub fn example() -> Highlighted {
        Highlighted { file: 1, highlight: 9, lines: 3, omitted: 0 }
    }
}

impl BookmarkAdded {
    pub fn example() -> BookmarkAdded {
        BookmarkAdded { file: 1, bookmark: 11, line: 48_213, name: "first 502".into() }
    }
}

impl Bookmarks {
    pub fn example() -> Bookmarks {
        let b = BookmarkOut {
            bookmark: 11,
            line: 48_213,
            name: "first 502".into(),
            note: Some("payments upstream timed out here".into()),
            color: "amber".into(),
            by: "agent".into(),
        };
        Bookmarks { file: 1, bookmarks: vec![b], omitted: 0 }
    }
}

impl Noted {
    pub fn example() -> Noted {
        Noted { note: 12 }
    }
}

impl Undone {
    pub fn example() -> Undone {
        Undone { undone: 4 }
    }
}

/// The example result of agent command `id` as JSON (`None` for other ids).
pub fn example(id: &str) -> Option<Value> {
    fn v<T: Serialize>(t: T) -> Option<Value> {
        serde_json::to_value(t).ok()
    }
    match id {
        ids::LOG_OPEN => v(Open::example()),
        ids::LOG_FILES => v(Files::example()),
        ids::LOG_STATS => v(Stats::example()),
        ids::LOG_SEARCH => v(Search::example()),
        ids::LOG_LINES => v(Lines::example()),
        ids::LOG_AROUND => v(Around::example()),
        ids::LOG_HISTOGRAM => v(HistogramOut::example()),
        ids::LOG_FIELDS => v(Fields::example()),
        ids::LOG_FIRST => v(First::example()),
        ids::VIEW_FILTER => v(Filtered::example()),
        ids::VIEW_CLEAR_FILTERS => v(Cleared::example()),
        ids::VIEW_GOTO => v(Goto::example()),
        ids::VIEW_HIGHLIGHT => v(Highlighted::example()),
        ids::BOOKMARK_ADD => v(BookmarkAdded::example()),
        ids::BOOKMARK_LIST => v(Bookmarks::example()),
        ids::AGENT_NOTE => v(Noted::example()),
        ids::AGENT_UNDO => v(Undone::example()),
        _ => None,
    }
}
