//! The command contract: every Strata command id and its parameters (frozen; change additively).
//! Owned by the lead. `commands/mod.rs` registers them with labels, menus and shortcuts.
//!
//! Conventions: `file` is a file id from `log.open` / `log.files` and defaults to the active file;
//! `line` is 1-based; times are RFC 3339 (a missing zone means UTC) and durations
//! `<int><ms|s|m|h|d>`; `filter` uses the filter grammar (`strata_core::filter`).

use ashlar::prelude::Param;

const MAX: i64 = i64::MAX;

const FILE: Param = Param::int_in("file", 1, MAX).about("file id (from log.open or log.files); default: the active file");
const FILTER_OPT: Param =
    Param::string("filter").about("only lines passing this filter (filter bar grammar, e.g. `level >= warn and service = api`)");
const LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error", "fatal"];
const FORMATS: &[&str] = &["plain", "jsonl", "logfmt", "syslog", "timestamped", "access"];

// ---------------------------------------------------------------------------------------------
// Agent commands (docs/PLAN.md §3, the MCP table). Results: crate::results.
// ---------------------------------------------------------------------------------------------

pub const LOG_OPEN: &str = "log.open";
pub const LOG_OPEN_PARAMS: &[Param] = &[
    Param::string("path").required().about("path of the log file (.log, .jsonl, .gz …)"),
    Param::bool("follow").about("follow the file as it grows (default false)"),
];

pub const LOG_FILES: &str = "log.files";
pub const LOG_FILES_PARAMS: &[Param] = &[];

pub const LOG_STATS: &str = "log.stats";
pub const LOG_STATS_PARAMS: &[Param] = &[FILE, Param::json("range").about(r#"only lines in this time range: {"from"?: time, "to"?: time}"#)];

pub const LOG_SEARCH: &str = "log.search";
pub const LOG_SEARCH_PARAMS: &[Param] = &[
    FILE,
    Param::string("query").required().about("text to find (a regex with regex: true)"),
    Param::bool("regex").about("query is a regular expression (default false)"),
    Param::bool("case").about("match case (default false)"),
    Param::bool("whole_word").about("match whole words only (default false)"),
    Param::int_in("limit", 1, 500).about("matches to return (default 50, max 500)"),
    FILTER_OPT,
];

pub const LOG_LINES: &str = "log.lines";
pub const LOG_LINES_PARAMS: &[Param] = &[
    FILE,
    Param::int_in("from", 1, MAX).required().about("first line (1-based)"),
    Param::int_in("count", 1, 500).required().about("lines to return (max 500)"),
];

pub const LOG_AROUND: &str = "log.around";
pub const LOG_AROUND_PARAMS: &[Param] = &[
    FILE,
    Param::int_in("line", 1, MAX).required().about("the line (1-based)"),
    Param::int_in("before", 0, 200).about("lines before it (default 10, max 200)"),
    Param::int_in("after", 0, 200).about("lines after it (default 10, max 200)"),
];

pub const LOG_HISTOGRAM: &str = "log.histogram";
pub const LOG_HISTOGRAM_PARAMS: &[Param] =
    &[FILE, Param::string("bucket").about("bucket width: `auto` (default) or a duration like 30s, 5m, 1h"), FILTER_OPT];

pub const LOG_FIELDS: &str = "log.fields";
pub const LOG_FIELDS_PARAMS: &[Param] = &[FILE];

pub const LOG_FIRST: &str = "log.first";
pub const LOG_FIRST_PARAMS: &[Param] =
    &[FILE, Param::string("filter").required().about("filter grammar, e.g. `level = error and time >= 2026-11-12T14:00:00Z`")];

pub const VIEW_FILTER: &str = "view.filter";
pub const VIEW_FILTER_PARAMS: &[Param] = &[FILE, Param::string("filter").required().about("filter to add as a chip (filter bar grammar)")];

pub const VIEW_CLEAR_FILTERS: &str = "view.clear_filters";
pub const VIEW_CLEAR_FILTERS_PARAMS: &[Param] = &[FILE];

pub const VIEW_GOTO: &str = "view.goto";
pub const VIEW_GOTO_PARAMS: &[Param] = &[
    FILE,
    Param::int_in("line", 1, MAX).about("line to scroll to (1-based); give line or time"),
    Param::string("time").about("scroll to the first line at or after this time"),
];

pub const VIEW_HIGHLIGHT: &str = "view.highlight";
pub const VIEW_HIGHLIGHT_PARAMS: &[Param] = &[
    FILE,
    Param::json("lines").about("array of 1-based line numbers (max 500); give lines or query"),
    Param::string("query").about("highlight the lines matching this text (first 500)"),
    Param::string("note").about("why: shown in the agent panel and on hover"),
];

pub const BOOKMARK_ADD: &str = "bookmark.add";
pub const BOOKMARK_ADD_PARAMS: &[Param] = &[
    FILE,
    Param::int_in("line", 1, MAX).about("line (1-based); default: the selected line"),
    Param::string("name").about("default: `Line <n>`"),
    Param::string("note"),
];

pub const BOOKMARK_LIST: &str = "bookmark.list";
pub const BOOKMARK_LIST_PARAMS: &[Param] = &[FILE];

pub const AGENT_NOTE: &str = "agent.note";
pub const AGENT_NOTE_PARAMS: &[Param] = &[Param::string("text").required().about("a note for the person, shown in the agent panel (max 8 KB)")];

pub const AGENT_UNDO: &str = "agent.undo";
pub const AGENT_UNDO_PARAMS: &[Param] = &[];

/// The agent commands, in the order of the MCP table. Each has a golden result file
/// `crates/app/tests/mcp/<id>.json` and a type in `crate::results`.
pub const AGENT: &[&str] = &[
    LOG_OPEN,
    LOG_FILES,
    LOG_STATS,
    LOG_SEARCH,
    LOG_LINES,
    LOG_AROUND,
    LOG_HISTOGRAM,
    LOG_FIELDS,
    LOG_FIRST,
    VIEW_FILTER,
    VIEW_CLEAR_FILTERS,
    VIEW_GOTO,
    VIEW_HIGHLIGHT,
    BOOKMARK_ADD,
    BOOKMARK_LIST,
    AGENT_NOTE,
    AGENT_UNDO,
];

/// Agent commands that only read (never change what the person sees); registered as queries.
pub const READ_ONLY: &[&str] = &[LOG_FILES, LOG_STATS, LOG_SEARCH, LOG_LINES, LOG_AROUND, LOG_HISTOGRAM, LOG_FIELDS, LOG_FIRST, BOOKMARK_LIST];

// ---------------------------------------------------------------------------------------------
// User commands (menus, palette, keyboard; callable over MCP too, but not part of the agent
// contract). docs/DESIGN.md, Keyboard map.
// ---------------------------------------------------------------------------------------------

pub const LOG_OPEN_DIALOG: &str = "log.open_dialog";
/// With `path`, opens that file without asking (drag and drop, recent files, `.ui` scripts);
/// without, the window asks with the native dialog. (`path` and `follow`: added by the ui stream.)
pub const LOG_OPEN_DIALOG_PARAMS: &[Param] = &[
    Param::string("path").about("open this file without asking; absent: show the open dialog"),
    Param::bool("follow").about("follow the file as it grows (default false)"),
];

pub const LOG_CLOSE: &str = "log.close";
pub const LOG_CLOSE_PARAMS: &[Param] = &[FILE];

pub const LOG_RELOAD: &str = "log.reload";
pub const LOG_RELOAD_PARAMS: &[Param] = &[FILE];

pub const LOG_STOP_INDEXING: &str = "log.stop_indexing";
pub const LOG_STOP_INDEXING_PARAMS: &[Param] = &[FILE];

pub const LOG_SET_FORMAT: &str = "log.set_format";
pub const LOG_SET_FORMAT_PARAMS: &[Param] = &[FILE, Param::one_of("format", FORMATS).required()];

pub const LOG_REDACTION: &str = "log.redaction";
pub const LOG_REDACTION_PARAMS: &[Param] = &[FILE, Param::bool("on").required().about("redact MCP results of this file")];

pub const SEARCH_FOCUS: &str = "search.focus";
pub const SEARCH_FOCUS_PARAMS: &[Param] = &[];

pub const SEARCH_RUN: &str = "search.run";
pub const SEARCH_RUN_PARAMS: &[Param] =
    &[FILE, Param::string("query").required(), Param::bool("regex"), Param::bool("case"), Param::bool("whole_word")];

pub const SEARCH_NEXT: &str = "search.next";
pub const SEARCH_NEXT_PARAMS: &[Param] = &[FILE];

pub const SEARCH_PREV: &str = "search.prev";
pub const SEARCH_PREV_PARAMS: &[Param] = &[FILE];

pub const SEARCH_CLEAR: &str = "search.clear";
pub const SEARCH_CLEAR_PARAMS: &[Param] = &[FILE];

pub const FILTER_FOCUS: &str = "filter.focus";
pub const FILTER_FOCUS_PARAMS: &[Param] = &[];

pub const FILTER_TOGGLE: &str = "filter.toggle";
pub const FILTER_TOGGLE_PARAMS: &[Param] = &[FILE, Param::int_in("id", 1, MAX).required().about("filter chip id")];

pub const FILTER_REMOVE: &str = "filter.remove";
pub const FILTER_REMOVE_PARAMS: &[Param] = &[FILE, Param::int_in("id", 1, MAX).required().about("filter chip id")];

pub const FILTER_LEVEL: &str = "filter.level";
pub const FILTER_LEVEL_PARAMS: &[Param] =
    &[FILE, Param::one_of("level", LEVELS).required(), Param::bool("on").about("show the level (default: toggle)")];

pub const FILTER_TIME_RANGE: &str = "filter.time_range";
pub const FILTER_TIME_RANGE_PARAMS: &[Param] =
    &[FILE, Param::string("from").about("time; absent: no lower bound"), Param::string("to").about("time; absent: no upper bound")];

pub const FILTER_SAVE: &str = "filter.save";
pub const FILTER_SAVE_PARAMS: &[Param] = &[FILE, Param::string("name").required().about("save the active filters under this name")];

pub const FILTER_APPLY_SAVED: &str = "filter.apply_saved";
pub const FILTER_APPLY_SAVED_PARAMS: &[Param] = &[FILE, Param::string("name").required()];

pub const FILTER_DELETE_SAVED: &str = "filter.delete_saved";
pub const FILTER_DELETE_SAVED_PARAMS: &[Param] = &[Param::string("name").required()];

pub const NAV_NEXT_ERROR: &str = "nav.next_error";
pub const NAV_PREV_ERROR: &str = "nav.prev_error";
pub const NAV_NEXT_WARNING: &str = "nav.next_warning";
pub const NAV_PREV_WARNING: &str = "nav.prev_warning";
pub const NAV_PARAMS: &[Param] = &[FILE];

pub const NAV_GOTO_PROMPT: &str = "nav.goto_prompt";
pub const NAV_GOTO_PROMPT_PARAMS: &[Param] = &[];

pub const VIEW_FOLLOW: &str = "view.follow";
pub const VIEW_FOLLOW_PARAMS: &[Param] = &[FILE, Param::bool("on").about("default: toggle")];

pub const VIEW_SELECT: &str = "view.select";
pub const VIEW_SELECT_PARAMS: &[Param] = &[FILE, Param::int_in("line", 1, MAX).required()];

pub const VIEW_TOGGLE_DETAIL: &str = "view.toggle_detail";
pub const VIEW_TOGGLE_AGENT_PANEL: &str = "view.toggle_agent_panel";
pub const VIEW_SHOW_BOOKMARKS: &str = "view.show_bookmarks";
pub const NO_PARAMS: &[Param] = &[];

pub const VIEW_TOGGLE_COLUMN: &str = "view.toggle_column";
pub const VIEW_TOGGLE_COLUMN_PARAMS: &[Param] = &[
    FILE,
    Param::string("column").required().about("`line`, `time`, `level`, `message` or a field name"),
    Param::bool("on").about("default: toggle"),
];

pub const VIEW_MOVE_COLUMN: &str = "view.move_column";
pub const VIEW_MOVE_COLUMN_PARAMS: &[Param] =
    &[FILE, Param::string("column").required(), Param::int_in("to", 0, 1000).required().about("new position, 0-based")];

pub const VIEW_RESIZE_COLUMN: &str = "view.resize_column";
pub const VIEW_RESIZE_COLUMN_PARAMS: &[Param] =
    &[FILE, Param::string("column").required(), Param::float_in("width", 0.0, 4000.0).about("points; absent: automatic")];

pub const BOOKMARK_REMOVE: &str = "bookmark.remove";
pub const BOOKMARK_REMOVE_PARAMS: &[Param] = &[FILE, Param::int_in("id", 1, MAX).required()];

pub const BOOKMARK_RENAME: &str = "bookmark.rename";
pub const BOOKMARK_RENAME_PARAMS: &[Param] = &[FILE, Param::int_in("id", 1, MAX).required(), Param::string("name").required()];

pub const HIGHLIGHT_REMOVE: &str = "highlight.remove";
pub const HIGHLIGHT_REMOVE_PARAMS: &[Param] = &[FILE, Param::int_in("id", 1, MAX).required()];

pub const DETAIL_COPY_LINE: &str = "detail.copy_line";
pub const DETAIL_COPY_JSON: &str = "detail.copy_json";

pub const DETAIL_FILTER_BY_VALUE: &str = "detail.filter_by_value";
pub const DETAIL_FILTER_BY_VALUE_PARAMS: &[Param] = &[FILE, Param::string("field").required(), Param::string("value").required()];

pub const PALETTE_OPEN: &str = "palette.open";

pub const SETTINGS_OPEN: &str = "settings.open";
pub const SETTINGS_OPEN_PARAMS: &[Param] = &[Param::one_of("section", &["general", "formats", "redaction", "ai"]).about("default: general")];

pub const AGENT_ALLOW_CHANGES: &str = "agent.allow_changes";
pub const AGENT_ALLOW_CHANGES_PARAMS: &[Param] = &[Param::bool("on").required()];

/// The user commands.
pub const USER: &[&str] = &[
    LOG_OPEN_DIALOG,
    LOG_CLOSE,
    LOG_RELOAD,
    LOG_STOP_INDEXING,
    LOG_SET_FORMAT,
    LOG_REDACTION,
    SEARCH_FOCUS,
    SEARCH_RUN,
    SEARCH_NEXT,
    SEARCH_PREV,
    SEARCH_CLEAR,
    FILTER_FOCUS,
    FILTER_TOGGLE,
    FILTER_REMOVE,
    FILTER_LEVEL,
    FILTER_TIME_RANGE,
    FILTER_SAVE,
    FILTER_APPLY_SAVED,
    FILTER_DELETE_SAVED,
    NAV_NEXT_ERROR,
    NAV_PREV_ERROR,
    NAV_NEXT_WARNING,
    NAV_PREV_WARNING,
    NAV_GOTO_PROMPT,
    VIEW_FOLLOW,
    VIEW_SELECT,
    VIEW_TOGGLE_DETAIL,
    VIEW_TOGGLE_AGENT_PANEL,
    VIEW_SHOW_BOOKMARKS,
    VIEW_TOGGLE_COLUMN,
    VIEW_MOVE_COLUMN,
    VIEW_RESIZE_COLUMN,
    BOOKMARK_REMOVE,
    BOOKMARK_RENAME,
    HIGHLIGHT_REMOVE,
    DETAIL_COPY_LINE,
    DETAIL_COPY_JSON,
    DETAIL_FILTER_BY_VALUE,
    PALETTE_OPEN,
    SETTINGS_OPEN,
    AGENT_ALLOW_CHANGES,
];
