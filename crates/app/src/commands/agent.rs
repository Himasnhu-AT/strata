//! Handlers of the agent (MCP) commands. Owned by the `agent` stream: fill in the bodies, keep
//! the signatures (they are registered in `commands/mod.rs`) and return the `crate::results`
//! types. View-changing commands record an `AgentChange` when the caller is an agent, and fail
//! with `crate::results::DENIED` when the person turned agent changes off.

use ashlar::prelude::*;

use super::{R, S, ids, not_yet};
use crate::doc::view_keys;

/// Enabled predicate of the view-changing agent commands: off when the person turned off
/// "Allow the agent to change the view" (`view_keys::AGENT_ALLOW_CHANGES`).
pub fn can_change_view(s: &S) -> Result<(), String> {
    let _ = s.view_or(view_keys::AGENT_ALLOW_CHANGES, true);
    Ok(())
}

/// Enabled predicate of `agent.undo`: there are agent changes since the person last acted.
pub fn has_agent_changes(s: &S) -> Result<(), String> {
    let _ = &s.doc().agent.pending;
    Ok(())
}

/// log.open: open a file (`OpenFile` + `Live`), start the index job; result `results::Open`.
pub fn log_open(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_OPEN)
}

/// log.files: `results::Files`.
pub fn log_files(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_FILES)
}

/// log.stats: `results::Stats`.
pub fn log_stats(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_STATS)
}

/// log.search: `results::Search` (limit 50, max 500; redacted, cut to 2 KB).
pub fn log_search(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_SEARCH)
}

/// log.lines: `results::Lines`.
pub fn log_lines(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_LINES)
}

/// log.around: `results::Around`.
pub fn log_around(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_AROUND)
}

/// log.histogram: `results::HistogramOut`.
pub fn log_histogram(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_HISTOGRAM)
}

/// log.fields: `results::Fields`.
pub fn log_fields(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_FIELDS)
}

/// log.first: `results::First`.
pub fn log_first(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_FIRST)
}

/// view.filter: add a `FilterChip` (label `ADD_FILTER`); `results::Filtered`.
pub fn view_filter(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_FILTER)
}

/// view.clear_filters: label `CLEAR_FILTERS`; `results::Cleared`.
pub fn view_clear_filters(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_CLEAR_FILTERS)
}

/// view.goto: sets `view_keys::SCROLL_TO` and `SELECTED_LINE` (no undo step); `results::Goto`.
pub fn view_goto(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_GOTO)
}

/// view.highlight: label `HIGHLIGHT`; `results::Highlighted`.
pub fn view_highlight(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_HIGHLIGHT)
}

/// bookmark.add: label `ADD_BOOKMARK`; `results::BookmarkAdded`.
pub fn bookmark_add(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::BOOKMARK_ADD)
}

/// bookmark.list: `results::Bookmarks`.
pub fn bookmark_list(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::BOOKMARK_LIST)
}

/// agent.note: label `AGENT_NOTE`; `results::Noted`.
pub fn agent_note(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::AGENT_NOTE)
}

/// agent.undo: one step labelled `UNDO_AGENT_CHANGES`; `results::Undone`.
pub fn agent_undo(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::AGENT_UNDO)
}
