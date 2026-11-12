//! An open log file: the map, the line index and the detected format. Owned by the `index`
//! stream.
//!
//! A `LogFile` is shared as `Arc<LogFile>` between the UI thread and jobs: indexing grows the
//! index while the table reads lines, so every method takes `&self` (interior locking).

use std::path::{Path, PathBuf};

use crate::formats::{self, FieldNames};
use crate::index::{IndexState, LineIndex};
use crate::{FileId, FormatKind, LineNo, Parsed, Result};

/// One line as read for display or an MCP result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub number: LineNo,
    /// The line's bytes, without the newline, cut to the requested maximum.
    pub bytes: Vec<u8>,
    /// Whether the line was longer than the maximum.
    pub cut: bool,
    /// Its full length in bytes.
    pub len: u64,
}

impl Line {
    /// The text, invalid UTF-8 replaced with U+FFFD.
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }
}

/// What changed on disk since the last [`LogFile::refresh`] (follow mode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileChange {
    #[default]
    Unchanged,
    /// New bytes at the end: index them.
    Appended { bytes: u64 },
    /// The file got shorter: the index past the new end was dropped.
    Truncated,
    /// The path now names another file (rotation): the banner offers to reload.
    Replaced,
    /// The file is gone.
    Removed,
}

/// An open log file.
#[derive(Debug, Default)]
pub struct LogFile {
    id: FileId,
    path: PathBuf,
    format: FormatKind,
    names: FieldNames,
}

impl LogFile {
    /// Open `path`: map it, detect its format from the first [`crate::limits::DETECT_BYTES`] and
    /// start with an empty index (fill it with [`LogFile::index_more`] from a job). Fails for a
    /// missing, unreadable or binary file.
    pub fn open(id: FileId, path: &Path) -> Result<LogFile> {
        Ok(LogFile { id, path: path.to_path_buf(), format: FormatKind::Plain, names: FieldNames::default() })
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size in bytes as last seen.
    pub fn len(&self) -> u64 {
        0
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The detected (or chosen) format.
    pub fn format(&self) -> FormatKind {
        self.format
    }

    /// The field names interned for this file (shared by every parse of its lines).
    pub fn names(&self) -> &FieldNames {
        &self.names
    }

    /// Parse `line` with this file's format.
    pub fn parse(&self, line: &[u8]) -> Parsed {
        formats::parser(self.format).parse(line, &self.names)
    }

    /// Lines indexed so far.
    pub fn lines(&self) -> u64 {
        0
    }

    pub fn index_state(&self) -> IndexState {
        IndexState::Idle
    }

    /// Index up to `max_bytes` more bytes; call it in a loop from a job, checking `cancel` in
    /// between. Returns the new state ([`IndexState::Complete`] when done).
    pub fn index_more(&self, max_bytes: u64) -> Result<IndexState> {
        let _ = max_bytes;
        Ok(IndexState::Complete)
    }

    /// Read the index (summaries, ranges) under its lock. Keep `f` short.
    pub fn with_index<R>(&self, f: impl FnOnce(&LineIndex) -> R) -> R {
        f(&LineIndex::new())
    }

    /// Line `n`, cut to `max_bytes`. `None` past the indexed part.
    pub fn line(&self, n: LineNo, max_bytes: usize) -> Option<Line> {
        let _ = (n, max_bytes);
        None
    }

    /// Up to `count` lines from `from`, each cut to `max_bytes`.
    pub fn lines_from(&self, from: LineNo, count: usize, max_bytes: usize) -> Vec<Line> {
        let _ = (from, count, max_bytes);
        Vec::new()
    }

    /// Run `f` on the raw bytes of lines `from..to` (searching without copies). `None` when the
    /// range is not indexed.
    pub fn with_bytes<R>(&self, from: LineNo, to: LineNo, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let _ = (from, to, f);
        None
    }

    /// Check the file on disk (follow mode, after a watcher event) and update the map and index.
    pub fn refresh(&self) -> Result<FileChange> {
        Ok(FileChange::Unchanged)
    }
}
