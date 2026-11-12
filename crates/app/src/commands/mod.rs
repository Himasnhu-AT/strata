//! Command registration: every id of [`ids`] with its label, menu, shortcut, parameters and
//! handler. Owned by the lead (frozen). Handlers live in [`agent`] (the `agent` stream) and
//! [`user`] (the `ui` stream); each stream fills in its bodies and enabled predicates without
//! touching this table.
//!
//! ashlar's `cmd!` / `query!` macros take literal ids, so the table uses `CommandSpec::new` with
//! the [`ids`] constants. Read-only agent commands get `.query()` (not journaled) with no menu and
//! the `always` predicate: exactly what `query!` builds.

pub mod agent;
pub mod ids;
pub mod user;

use ashlar::prelude::*;

use crate::doc::StrataDoc;

/// The session commands run on.
pub type S = Session<StrataDoc>;
/// A command's result.
pub type R = Result<Value, CommandError>;

/// The stub body: `CommandError::failed(id, "not yet")`.
pub(crate) fn not_yet(id: &str) -> R {
    Err(CommandError::failed(id, "not yet"))
}

fn spec(id: &'static str, label: &'static str, run: ashlar::command::Run<S>, params: &'static [Param]) -> CommandSpec<S> {
    CommandSpec::new(id, label, run).params(Params::Static(params))
}

/// Every Strata command, after ashlar's built-in File and Edit ones.
///
/// Shortcut notes (ashlar 0.2): `file.open` (`Mod+O`) is ashlar's built-in "open a workspace", so
/// opening a log is `Mod+Shift+O`; the palette stays on ashlar's `Mod+Shift+P` and `palette.open`
/// adds `Mod+K`.
pub fn commands() -> Vec<CommandSpec<S>> {
    use ids::*;
    vec![
        // --- user commands ---------------------------------------------------------------------
        spec(LOG_OPEN_DIALOG, "Open Log…", user::log_open_dialog, LOG_OPEN_DIALOG_PARAMS).menu(&["File"]).shortcut("Mod+Shift+O"),
        spec(LOG_CLOSE, "Close Log", user::log_close, LOG_CLOSE_PARAMS).menu(&["File"]).shortcut("Mod+W").enabled(user::has_file),
        spec(LOG_RELOAD, "Reload Log", user::log_reload, LOG_RELOAD_PARAMS).menu(&["File"]).enabled(user::has_file),
        spec(LOG_STOP_INDEXING, "Stop Indexing", user::log_stop_indexing, LOG_STOP_INDEXING_PARAMS).menu(&["File"]).enabled(user::is_indexing),
        spec(LOG_SET_FORMAT, "Set Format…", user::log_set_format, LOG_SET_FORMAT_PARAMS).menu(&["View"]).enabled(user::has_file),
        spec(SEARCH_FOCUS, "Find…", user::search_focus, SEARCH_FOCUS_PARAMS).menu(&["Search"]).shortcut("Mod+F"),
        spec(SEARCH_RUN, "Search", user::search_run, SEARCH_RUN_PARAMS).enabled(user::has_file),
        spec(SEARCH_NEXT, "Next Match", user::search_next, SEARCH_NEXT_PARAMS).menu(&["Search"]).shortcut("F3").enabled(user::has_search),
        spec(SEARCH_PREV, "Previous Match", user::search_prev, SEARCH_PREV_PARAMS).menu(&["Search"]).shortcut("Shift+F3").enabled(user::has_search),
        spec(SEARCH_CLEAR, "Clear Search", user::search_clear, SEARCH_CLEAR_PARAMS).menu(&["Search"]).enabled(user::has_search),
        spec(FILTER_FOCUS, "Filter…", user::filter_focus, FILTER_FOCUS_PARAMS).menu(&["Search"]).shortcut("Mod+L"),
        spec(FILTER_TOGGLE, "Toggle Filter", user::filter_toggle, FILTER_TOGGLE_PARAMS).enabled(user::has_file),
        spec(FILTER_REMOVE, "Remove Filter", user::filter_remove, FILTER_REMOVE_PARAMS).enabled(user::has_file),
        spec(FILTER_LEVEL, "Show or Hide Level", user::filter_level, FILTER_LEVEL_PARAMS).enabled(user::has_file),
        spec(FILTER_TIME_RANGE, "Set Time Range", user::filter_time_range, FILTER_TIME_RANGE_PARAMS).menu(&["Search"]).enabled(user::has_file),
        spec(FILTER_SAVE, "Save Filters…", user::filter_save, FILTER_SAVE_PARAMS).menu(&["Search"]).enabled(user::has_file),
        spec(FILTER_APPLY_SAVED, "Apply Saved Filter", user::filter_apply_saved, FILTER_APPLY_SAVED_PARAMS).enabled(user::has_file),
        spec(FILTER_DELETE_SAVED, "Delete Saved Filter", user::filter_delete_saved, FILTER_DELETE_SAVED_PARAMS),
        spec(NAV_NEXT_ERROR, "Next Error", user::nav_next_error, NAV_PARAMS).menu(&["Go"]).shortcut("E").enabled(user::has_file),
        spec(NAV_PREV_ERROR, "Previous Error", user::nav_prev_error, NAV_PARAMS).menu(&["Go"]).shortcut("Shift+E").enabled(user::has_file),
        spec(NAV_NEXT_WARNING, "Next Warning", user::nav_next_warning, NAV_PARAMS).menu(&["Go"]).shortcut("W").enabled(user::has_file),
        spec(NAV_PREV_WARNING, "Previous Warning", user::nav_prev_warning, NAV_PARAMS).menu(&["Go"]).shortcut("Shift+W").enabled(user::has_file),
        spec(NAV_GOTO_PROMPT, "Go to Line or Time…", user::nav_goto_prompt, NAV_GOTO_PROMPT_PARAMS)
            .menu(&["Go"])
            .shortcut("Mod+G")
            .enabled(user::has_file),
        spec(VIEW_FOLLOW, "Follow", user::view_follow, VIEW_FOLLOW_PARAMS).menu(&["View"]).shortcut("F").enabled(user::has_file),
        spec(VIEW_SELECT, "Select Line", user::view_select, VIEW_SELECT_PARAMS).enabled(user::has_file),
        spec(VIEW_TOGGLE_DETAIL, "Detail Pane", user::view_toggle_detail, NO_PARAMS).menu(&["View"]).shortcut("Space"),
        spec(VIEW_TOGGLE_AGENT_PANEL, "Agent Panel", user::view_toggle_agent_panel, NO_PARAMS).menu(&["View"]).shortcut("Mod+Shift+A"),
        spec(VIEW_SHOW_BOOKMARKS, "Show Bookmarks", user::view_show_bookmarks, NO_PARAMS).menu(&["Bookmarks"]).shortcut("Mod+B"),
        spec(VIEW_TOGGLE_COLUMN, "Show or Hide Column", user::view_toggle_column, VIEW_TOGGLE_COLUMN_PARAMS).enabled(user::has_file),
        spec(VIEW_MOVE_COLUMN, "Move Column", user::view_move_column, VIEW_MOVE_COLUMN_PARAMS).enabled(user::has_file),
        spec(VIEW_RESIZE_COLUMN, "Resize Column", user::view_resize_column, VIEW_RESIZE_COLUMN_PARAMS).enabled(user::has_file),
        spec(BOOKMARK_REMOVE, "Remove Bookmark", user::bookmark_remove, BOOKMARK_REMOVE_PARAMS).enabled(user::has_file),
        spec(BOOKMARK_RENAME, "Rename Bookmark", user::bookmark_rename, BOOKMARK_RENAME_PARAMS).enabled(user::has_file),
        spec(HIGHLIGHT_REMOVE, "Remove Highlight", user::highlight_remove, HIGHLIGHT_REMOVE_PARAMS).enabled(user::has_file),
        spec(DETAIL_COPY_LINE, "Copy Line", user::detail_copy_line, NO_PARAMS).menu(&["Edit"]).enabled(user::has_selection),
        spec(DETAIL_COPY_JSON, "Copy as JSON", user::detail_copy_json, NO_PARAMS).menu(&["Edit"]).enabled(user::has_selection),
        spec(DETAIL_FILTER_BY_VALUE, "Filter by This Value", user::detail_filter_by_value, DETAIL_FILTER_BY_VALUE_PARAMS).enabled(user::has_file),
        spec(PALETTE_OPEN, "Command Palette", user::palette_open, NO_PARAMS).shortcut("Mod+K"),
        spec(SETTINGS_OPEN, "Settings…", user::settings_open, SETTINGS_OPEN_PARAMS).menu(&["View"]).shortcut("Mod+,"),
        spec(AGENT_ALLOW_CHANGES, "Allow Agent Changes", user::agent_allow_changes, AGENT_ALLOW_CHANGES_PARAMS).menu(&["Agent"]),
        spec(LOG_REDACTION, "Redact Agent Results", user::log_redaction, LOG_REDACTION_PARAMS).menu(&["Agent"]).enabled(user::has_file),
        // --- agent commands (MCP table) -----------------------------------------------------
        spec(LOG_OPEN, "Open Log Path", agent::log_open, LOG_OPEN_PARAMS),
        spec(LOG_FILES, "List Open Logs", agent::log_files, LOG_FILES_PARAMS).query(),
        spec(LOG_STATS, "Log Statistics", agent::log_stats, LOG_STATS_PARAMS).query(),
        spec(LOG_SEARCH, "Search Log", agent::log_search, LOG_SEARCH_PARAMS).query(),
        spec(LOG_LINES, "Read Lines", agent::log_lines, LOG_LINES_PARAMS).query(),
        spec(LOG_AROUND, "Read Lines Around", agent::log_around, LOG_AROUND_PARAMS).query(),
        spec(LOG_HISTOGRAM, "Time Histogram", agent::log_histogram, LOG_HISTOGRAM_PARAMS).query(),
        spec(LOG_FIELDS, "List Fields", agent::log_fields, LOG_FIELDS_PARAMS).query(),
        spec(LOG_FIRST, "Find First Line", agent::log_first, LOG_FIRST_PARAMS).query(),
        spec(VIEW_FILTER, "Add Filter", agent::view_filter, VIEW_FILTER_PARAMS).enabled(agent::can_change_view),
        spec(VIEW_CLEAR_FILTERS, "Clear Filters", agent::view_clear_filters, VIEW_CLEAR_FILTERS_PARAMS)
            .menu(&["View"])
            .enabled(agent::can_change_view),
        spec(VIEW_GOTO, "Go to Line or Time", agent::view_goto, VIEW_GOTO_PARAMS).enabled(agent::can_change_view),
        spec(VIEW_HIGHLIGHT, "Highlight Lines", agent::view_highlight, VIEW_HIGHLIGHT_PARAMS).enabled(agent::can_change_view),
        spec(BOOKMARK_ADD, "Add Bookmark", agent::bookmark_add, BOOKMARK_ADD_PARAMS)
            .menu(&["Bookmarks"])
            .shortcut("B")
            .enabled(agent::can_change_view),
        spec(BOOKMARK_LIST, "List Bookmarks", agent::bookmark_list, BOOKMARK_LIST_PARAMS).query(),
        spec(AGENT_NOTE, "Agent Note", agent::agent_note, AGENT_NOTE_PARAMS).enabled(agent::can_change_view),
        spec(AGENT_UNDO, "Undo Agent Changes", agent::agent_undo, AGENT_UNDO_PARAMS).menu(&["Agent"]).enabled(agent::has_agent_changes),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_is_registered_once_with_its_params() {
        let specs = commands();
        let all: Vec<&str> = ids::AGENT.iter().chain(ids::USER).copied().collect();
        assert_eq!(specs.len(), all.len());
        for id in &all {
            assert_eq!(specs.iter().filter(|s| s.id == *id).count(), 1, "{id}");
        }
        for s in &specs {
            assert_eq!(!s.mutates, ids::READ_ONLY.contains(&s.id), "{}: read-only commands are queries", s.id);
        }
    }
}
