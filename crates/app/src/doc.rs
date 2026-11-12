//! `StrataDoc`, the ashlar document: the workspace (open files by path, filters, columns,
//! bookmarks, highlights, agent changes), never the log text. Serializable, so the workspace
//! restores after a crash and can be saved (File ▸ Save, ashlar's built-in commands). Every change
//! is one `s.edit(labels::…, …)`: one undo step with a frozen label. Owned by the lead.
//!
//! Line numbers stored here are 0-based (`strata_core::LineNo`), like the core; the UI and MCP add
//! one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use strata_core::{FileId, Level, LineNo, LogFile, Timestamp};

/// The undo labels (the Edit menu shows "Undo <label>"). One per kind of change; agent changes use
/// the same labels (the agent panel tells them apart by [`Actor`]).
pub mod labels {
    pub const OPEN_LOG: &str = "Open Log";
    pub const CLOSE_LOG: &str = "Close Log";
    pub const SET_FORMAT: &str = "Set Format";
    pub const FOLLOW: &str = "Follow";
    pub const ADD_FILTER: &str = "Add Filter";
    pub const TOGGLE_FILTER: &str = "Toggle Filter";
    pub const REMOVE_FILTER: &str = "Remove Filter";
    pub const CLEAR_FILTERS: &str = "Clear Filters";
    pub const TOGGLE_LEVEL: &str = "Toggle Level";
    pub const TIME_RANGE: &str = "Set Time Range";
    pub const SAVE_FILTER: &str = "Save Filter";
    pub const APPLY_SAVED_FILTER: &str = "Apply Saved Filter";
    pub const DELETE_SAVED_FILTER: &str = "Delete Saved Filter";
    pub const ADD_BOOKMARK: &str = "Add Bookmark";
    pub const RENAME_BOOKMARK: &str = "Rename Bookmark";
    pub const REMOVE_BOOKMARK: &str = "Remove Bookmark";
    pub const HIGHLIGHT: &str = "Highlight";
    pub const REMOVE_HIGHLIGHT: &str = "Remove Highlight";
    pub const TOGGLE_COLUMN: &str = "Toggle Column";
    pub const MOVE_COLUMN: &str = "Move Column";
    /// Merged while dragging (`edit_merged` with key `column-<file>-<column>`).
    pub const RESIZE_COLUMN: &str = "Resize Column";
    pub const REDACTION: &str = "Redaction";
    pub const AGENT_NOTE: &str = "Agent Note";
    /// `agent.undo`: one step that removes every agent change since the person last acted.
    pub const UNDO_AGENT_CHANGES: &str = "Undo Agent Changes";

    /// Every label, for tests and docs.
    pub const ALL: &[&str] = &[
        OPEN_LOG,
        CLOSE_LOG,
        SET_FORMAT,
        FOLLOW,
        ADD_FILTER,
        TOGGLE_FILTER,
        REMOVE_FILTER,
        CLEAR_FILTERS,
        TOGGLE_LEVEL,
        TIME_RANGE,
        SAVE_FILTER,
        APPLY_SAVED_FILTER,
        DELETE_SAVED_FILTER,
        ADD_BOOKMARK,
        RENAME_BOOKMARK,
        REMOVE_BOOKMARK,
        HIGHLIGHT,
        REMOVE_HIGHLIGHT,
        TOGGLE_COLUMN,
        MOVE_COLUMN,
        RESIZE_COLUMN,
        REDACTION,
        AGENT_NOTE,
        UNDO_AGENT_CHANGES,
    ];
}

/// View-state keys (`s.view` / `s.set_view`: not in the document, not undoable, not saved). The
/// agent's `ui.set` control method can set them too.
pub mod view_keys {
    /// `u64` file id: the file shown in the window.
    pub const ACTIVE_FILE: &str = "active_file";
    /// `u64` 0-based line selected in the active file.
    pub const SELECTED_LINE: &str = "selected_line";
    /// `u64` 0-based line to scroll to once (the table clears it).
    pub const SCROLL_TO: &str = "scroll_to";
    /// `bool`: the detail pane is shown (default true).
    pub const DETAIL_OPEN: &str = "detail_open";
    /// `bool`: the agent panel is shown.
    pub const AGENT_PANEL_OPEN: &str = "agent_panel_open";
    /// `bool`: "Allow the agent to change the view" (default true; mirrored from Settings ▸ AI).
    pub const AGENT_ALLOW_CHANGES: &str = "agent_allow_changes";
    /// `string`: the panel or field to focus next frame: `search`, `filter`, `goto`, `bookmarks`.
    pub const FOCUS: &str = "focus";
    /// `string`: Settings section open (`general`, `formats`, `redaction`, `ai`); absent: closed.
    pub const SETTINGS: &str = "settings";
    /// The running search of the active file: `{"query", "regex", "case", "whole_word"}`.
    pub const SEARCH: &str = "search";
    /// The agent timeline: an array of `{"command", "summary", "changed_view", "at"}`, newest last
    /// (read-only calls included; capped at 500 rows).
    pub const AGENT_TIMELINE: &str = "agent_timeline";
    /// `string`: the agent client's name ("Claude"); absent when no agent is connected.
    pub const AGENT_CLIENT: &str = "agent_client";
    /// `string`: the screen a scene asked for, for screens that are not document state
    /// (`palette`, `settings`); the view reads it once.
    pub const SCENE: &str = "scene";
}

/// Who made a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    #[default]
    User,
    Agent,
}

/// A bookmark colour (the palette of docs/DESIGN.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkColor {
    #[default]
    Amber,
    Red,
    Yellow,
    Blue,
    Green,
    Grey,
}

impl BookmarkColor {
    /// The word in `bookmark.list`.
    pub fn as_str(self) -> &'static str {
        match self {
            BookmarkColor::Amber => "amber",
            BookmarkColor::Red => "red",
            BookmarkColor::Yellow => "yellow",
            BookmarkColor::Blue => "blue",
            BookmarkColor::Green => "green",
            BookmarkColor::Grey => "grey",
        }
    }
}

/// A filter chip in the toolbar. `text` is the canonical filter text (`Filter::to_text`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterChip {
    pub id: u64,
    pub text: String,
    pub enabled: bool,
    pub by: Actor,
}

/// A table column: `line`, `time`, `level`, `message`, or a parsed field name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    pub visible: bool,
    /// Width in points; `None`: automatic.
    pub width: Option<f32>,
}

/// A bookmark.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub line: LineNo,
    pub name: String,
    pub note: Option<String>,
    pub color: BookmarkColor,
    pub by: Actor,
}

/// Highlighted lines (agent highlights get an amber mark and an "AI" tag).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub id: u64,
    /// Sorted, at most 500.
    pub lines: Vec<LineNo>,
    /// The query that found them, when highlighted by query.
    pub query: Option<String>,
    pub note: Option<String>,
    pub by: Actor,
}

/// One open file (a tab): what to reopen and how it is viewed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenFile {
    pub id: FileId,
    pub path: PathBuf,
    pub follow: bool,
    /// A format chosen by the person (`FormatKind::as_str`); `None`: detected.
    pub format: Option<String>,
    pub filters: Vec<FilterChip>,
    /// Levels turned off with the level chips.
    pub hidden_levels: Vec<Level>,
    /// From the histogram drag (`None`: no limit on that side).
    pub time_from: Option<Timestamp>,
    pub time_to: Option<Timestamp>,
    pub columns: Vec<Column>,
    pub bookmarks: Vec<Bookmark>,
    pub highlights: Vec<Highlight>,
    /// Redaction of MCP results for this file (off shows a warning).
    pub redact: bool,
}

/// A named filter in the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedFilter {
    pub name: String,
    pub text: String,
}

/// A note from the agent (`agent.note`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentNote {
    pub id: u64,
    pub text: String,
}

/// Something the agent added since the person last acted: what `agent.undo` removes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AgentChange {
    Filter { file: FileId, id: u64 },
    Highlight { file: FileId, id: u64 },
    Bookmark { file: FileId, id: u64 },
    Note { id: u64 },
}

/// The agent's part of the workspace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub notes: Vec<AgentNote>,
    /// Agent changes since the person's last own change (cleared when the person acts).
    pub pending: Vec<AgentChange>,
}

/// The document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StrataDoc {
    /// Open files, in tab order.
    pub files: Vec<OpenFile>,
    /// The next id for files, filter chips, bookmarks, highlights and notes (one counter, so ids
    /// never repeat in a workspace). Start at 1.
    pub next_id: u64,
    pub saved_filters: Vec<SavedFilter>,
    pub agent: AgentState,
    /// The open `LogFile`s (runtime only, never saved).
    #[serde(skip)]
    pub live: Live,
}

impl StrataDoc {
    /// The open file `id`.
    pub fn file(&self, id: FileId) -> Option<&OpenFile> {
        self.files.iter().find(|f| f.id == id)
    }

    /// The open file `id`, to change inside an edit.
    pub fn file_mut(&mut self, id: FileId) -> Option<&mut OpenFile> {
        self.files.iter_mut().find(|f| f.id == id)
    }

    /// Take the next id.
    pub fn take_id(&mut self) -> u64 {
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

/// The runtime table of open [`LogFile`]s, shared by every revision of a session's document
/// (cloning shares it). Not part of the saved workspace: after a restore, files are reopened from
/// [`OpenFile::path`]. Equality ignores it.
#[derive(Clone, Debug, Default)]
pub struct Live(Arc<Mutex<HashMap<FileId, Arc<LogFile>>>>);

impl Live {
    /// The open `LogFile` of `id`.
    pub fn get(&self, id: FileId) -> Option<Arc<LogFile>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(&id).cloned()
    }

    /// Keep `file` open under its id.
    pub fn insert(&self, file: Arc<LogFile>) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(file.id(), file);
    }

    /// Drop the `LogFile` of `id` (its map is released when the last job holding it ends).
    pub fn remove(&self, id: FileId) -> Option<Arc<LogFile>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&id)
    }
}

impl PartialEq for Live {
    fn eq(&self, _: &Live) -> bool {
        true
    }
}

/// The default columns of a new file.
pub fn default_columns() -> Vec<Column> {
    ["line", "time", "level", "message"].into_iter().map(|k| Column { key: k.into(), visible: true, width: None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_round_trips_without_live_files() {
        let mut d = StrataDoc::default();
        let id = FileId(d.take_id());
        d.live.insert(Arc::new(LogFile::default()));
        d.files.push(OpenFile { id, path: "/tmp/a.log".into(), columns: default_columns(), redact: true, ..OpenFile::default() });
        let json = serde_json::to_string(&d).unwrap();
        let back: StrataDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        assert!(back.live.get(FileId(0)).is_none());
        assert_eq!(d.take_id(), 2);
    }
}
