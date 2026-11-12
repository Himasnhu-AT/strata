//! What user commands ask of the window. A command runs on the `Session` and cannot start a job
//! (ashlar 0.2), touch the clipboard or show a native dialog, so it leaves a [`Request`] in view
//! state ([`keys::REQUESTS`]); the view takes them at the start of the next frame and does the
//! work (jobs, dialogs, clipboard). Without a window (CLI, MCP) nobody takes them: the list is
//! capped, and commands that can answer inline do so there.

use serde::{Deserialize, Serialize};

use crate::commands::S;

/// View-state keys private to the window (not part of the contract; `ui.set` may still set
/// them).
pub mod keys {
    /// `[Request]`: work for the view, oldest first.
    pub const REQUESTS: &str = "strata.ui.requests";
    /// `{"path", "reason", "detail"}`: a file that could not be opened (the "Unreadable" dialog).
    pub const OPEN_ERROR: &str = "strata.ui.open_error";
    /// `bool`: start no background jobs (the `indexing` scene freezes a file part-way).
    pub const HOLD_JOBS: &str = "strata.ui.hold_jobs";
    /// `{"file", "change"}`: the followed file was replaced or removed (`replaced` / `removed`):
    /// the banner "The file was replaced. Reload?".
    pub const FILE_CHANGED: &str = "strata.ui.file_changed";
}

/// What a line search looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Error,
    Warning,
    /// The first line at or after `time` (go to time).
    Time,
}

/// One request from a command to the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Request {
    /// Show the native open dialog, then open the file picked.
    OpenDialog,
    /// Cancel the index job of `file`.
    StopIndexing { file: u64 },
    /// Select the next (or previous) line of `file` matching `target`, from line `from`
    /// (0-based; the selection, or the top). `time`: µs since the epoch, for [`Target::Time`].
    Find { file: u64, target: Target, back: bool, from: Option<u64>, time: Option<i64> },
    /// Select the next (or previous) search match.
    StepMatch { file: u64, back: bool },
    /// Put `text` on the clipboard.
    Copy { text: String },
}

/// Most requests kept for the view (a CLI session never takes them).
pub const MAX_REQUESTS: usize = 32;

/// Leave `r` for the view.
pub fn push(s: &mut S, r: Request) {
    let mut all: Vec<Request> = s.view(keys::REQUESTS).unwrap_or_default();
    if all.len() >= MAX_REQUESTS {
        all.remove(0);
    }
    all.push(r);
    s.set_view(keys::REQUESTS, &all);
}

/// Take every waiting request.
pub fn take(s: &mut S) -> Vec<Request> {
    let all: Vec<Request> = s.view(keys::REQUESTS).unwrap_or_default();
    if !all.is_empty() {
        s.set_view_value(keys::REQUESTS, serde_json::Value::Null);
    }
    all
}
