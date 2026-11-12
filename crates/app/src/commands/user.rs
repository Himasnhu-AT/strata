//! Handlers of the user commands (menus, palette, keyboard). Owned by the `ui` stream: fill in the
//! bodies and predicates, keep the signatures (registered in `commands/mod.rs`).

use ashlar::prelude::*;

use super::{R, S, ids, not_yet};
use crate::doc::view_keys;

/// Enabled when a file is open.
pub fn has_file(s: &S) -> Result<(), String> {
    let _ = &s.doc().files;
    Ok(())
}

/// Enabled while the active file is indexing.
pub fn is_indexing(s: &S) -> Result<(), String> {
    has_file(s)
}

/// Enabled when the active file has a search.
pub fn has_search(s: &S) -> Result<(), String> {
    let _ = s.view_state().get(view_keys::SEARCH);
    Ok(())
}

/// Enabled when a line is selected.
pub fn has_selection(s: &S) -> Result<(), String> {
    let _ = s.view::<u64>(view_keys::SELECTED_LINE);
    Ok(())
}

/// Ask for a log file (the view shows the native dialog, then dispatches `log.open`).
pub fn log_open_dialog(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_OPEN_DIALOG)
}

/// Close a file (label `CLOSE_LOG`).
pub fn log_close(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_CLOSE)
}

/// Reopen a file from disk (after "The file was replaced. Reload?").
pub fn log_reload(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_RELOAD)
}

/// Cancel the index job of a file (Esc in the status bar).
pub fn log_stop_indexing(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_STOP_INDEXING)
}

/// Override the detected format (label `SET_FORMAT`).
pub fn log_set_format(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_SET_FORMAT)
}

/// Turn redaction of agent results on or off for a file (label `REDACTION`).
pub fn log_redaction(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::LOG_REDACTION)
}

/// Focus the search field (`view_keys::FOCUS` = `search`).
pub fn search_focus(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SEARCH_FOCUS)
}

/// Start a search job; matches stream into the match list (`view_keys::SEARCH`).
pub fn search_run(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SEARCH_RUN)
}

/// Select the next match.
pub fn search_next(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SEARCH_NEXT)
}

/// Select the previous match.
pub fn search_prev(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SEARCH_PREV)
}

/// Clear the search.
pub fn search_clear(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SEARCH_CLEAR)
}

/// Focus the filter bar.
pub fn filter_focus(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_FOCUS)
}

/// Turn a filter chip on or off (label `TOGGLE_FILTER`).
pub fn filter_toggle(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_TOGGLE)
}

/// Remove a filter chip (label `REMOVE_FILTER`).
pub fn filter_remove(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_REMOVE)
}

/// Show or hide a level (label `TOGGLE_LEVEL`).
pub fn filter_level(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_LEVEL)
}

/// Limit to a time range (label `TIME_RANGE`).
pub fn filter_time_range(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_TIME_RANGE)
}

/// Save the active filters by name (label `SAVE_FILTER`).
pub fn filter_save(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_SAVE)
}

/// Apply a saved filter (label `APPLY_SAVED_FILTER`).
pub fn filter_apply_saved(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_APPLY_SAVED)
}

/// Delete a saved filter (label `DELETE_SAVED_FILTER`).
pub fn filter_delete_saved(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::FILTER_DELETE_SAVED)
}

/// Select the next error line.
pub fn nav_next_error(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::NAV_NEXT_ERROR)
}

/// Select the previous error line.
pub fn nav_prev_error(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::NAV_PREV_ERROR)
}

/// Select the next warning line.
pub fn nav_next_warning(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::NAV_NEXT_WARNING)
}

/// Select the previous warning line.
pub fn nav_prev_warning(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::NAV_PREV_WARNING)
}

/// Open the go-to-line-or-time field.
pub fn nav_goto_prompt(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::NAV_GOTO_PROMPT)
}

/// Follow a file or stop (label `FOLLOW`).
pub fn view_follow(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_FOLLOW)
}

/// Select a line (view state, no undo step).
pub fn view_select(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_SELECT)
}

/// Show or hide the detail pane.
pub fn view_toggle_detail(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_TOGGLE_DETAIL)
}

/// Show or hide the agent panel.
pub fn view_toggle_agent_panel(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_TOGGLE_AGENT_PANEL)
}

/// Show the Bookmarks section of the sidebar.
pub fn view_show_bookmarks(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_SHOW_BOOKMARKS)
}

/// Show or hide a column (label `TOGGLE_COLUMN`).
pub fn view_toggle_column(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_TOGGLE_COLUMN)
}

/// Move a column (label `MOVE_COLUMN`).
pub fn view_move_column(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_MOVE_COLUMN)
}

/// Resize a column (label `RESIZE_COLUMN`, merged while dragging).
pub fn view_resize_column(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::VIEW_RESIZE_COLUMN)
}

/// Remove a bookmark (label `REMOVE_BOOKMARK`).
pub fn bookmark_remove(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::BOOKMARK_REMOVE)
}

/// Rename a bookmark (label `RENAME_BOOKMARK`).
pub fn bookmark_rename(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::BOOKMARK_RENAME)
}

/// Remove a highlight (label `REMOVE_HIGHLIGHT`).
pub fn highlight_remove(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::HIGHLIGHT_REMOVE)
}

/// Copy the selected line.
pub fn detail_copy_line(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::DETAIL_COPY_LINE)
}

/// Copy the selected line as pretty JSON.
pub fn detail_copy_json(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::DETAIL_COPY_JSON)
}

/// Add a `field = value` filter chip (label `ADD_FILTER`).
pub fn detail_filter_by_value(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::DETAIL_FILTER_BY_VALUE)
}

/// Open the command palette (`Effect::OpenPalette`).
pub fn palette_open(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::PALETTE_OPEN)
}

/// Open Settings at a section (`view_keys::SETTINGS`).
pub fn settings_open(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::SETTINGS_OPEN)
}

/// Allow or forbid agent changes (`view_keys::AGENT_ALLOW_CHANGES`, saved in Settings).
pub fn agent_allow_changes(s: &mut S, p: &Value) -> R {
    let _ = (s, p);
    not_yet(ids::AGENT_ALLOW_CHANGES)
}
