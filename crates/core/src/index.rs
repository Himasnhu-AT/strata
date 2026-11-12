//! The line index: where each line starts, in chunks of [`CHUNK_LINES`] lines, with per-chunk
//! summaries that let time filters, the histogram and "first error after T" skip whole chunks.
//! Owned by the `index` stream.
//!
//! Offsets are kept as `Vec<u64>` per chunk; chunks past a memory budget are stored compressed
//! as deltas (2–3 bytes a line). The index is built incrementally by [`crate::LogFile::index_more`]
//! and can be read while it grows.

use std::ops::Range;

use crate::limits::CHUNK_LINES;
use crate::{LevelCounts, LineNo, Timestamp};

/// Where indexing is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum IndexState {
    /// Not started.
    #[default]
    Idle,
    /// Running: `done` of `total` bytes indexed.
    Running { done: u64, total: u64 },
    /// Every byte up to the file's length is indexed (following can grow it again).
    Complete,
    /// Stopped by the person (Esc) or a close; what is indexed stays usable.
    Cancelled,
    /// Stopped by an error (the text is shown in the status bar).
    Failed(String),
}

impl IndexState {
    /// The word used in MCP results: `idle`, `running`, `complete`, `cancelled`, `failed`.
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexState::Idle => "idle",
            IndexState::Running { .. } => "running",
            IndexState::Complete => "complete",
            IndexState::Cancelled => "cancelled",
            IndexState::Failed(_) => "failed",
        }
    }
}

/// What one chunk of [`CHUNK_LINES`] lines holds, without reading it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkSummary {
    /// The chunk's first line.
    pub first_line: LineNo,
    /// Lines in the chunk (the last chunk may be partial).
    pub lines: u32,
    /// Byte range of the chunk in the file.
    pub bytes: Range<u64>,
    /// Earliest and latest timestamp of its lines (`None`: no line has one, or not parsed yet).
    pub min_time: Option<Timestamp>,
    pub max_time: Option<Timestamp>,
    /// Lines per level (filled when the chunk is parsed for summaries; zero until then).
    pub levels: LevelCounts,
    /// Whether `min_time`, `max_time` and `levels` are filled in.
    pub summarized: bool,
}

/// Line start offsets for one file.
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
    lines: u64,
    indexed_bytes: u64,
}

impl LineIndex {
    /// An empty index.
    pub fn new() -> LineIndex {
        LineIndex::default()
    }

    /// Lines indexed so far. A last line without a newline counts once the file is complete.
    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Bytes covered by the index so far.
    pub fn indexed_bytes(&self) -> u64 {
        self.indexed_bytes
    }

    /// Byte range of line `line`, without its `\n` (or `\r\n`). `None` past the indexed part.
    pub fn line_range(&self, line: LineNo) -> Option<Range<u64>> {
        let _ = line;
        None
    }

    /// The line that contains byte `offset`.
    pub fn line_at_offset(&self, offset: u64) -> Option<LineNo> {
        let _ = offset;
        None
    }

    /// Number of chunks (the last may be partial).
    pub fn chunks(&self) -> usize {
        0
    }

    /// The summary of chunk `i`.
    pub fn chunk(&self, i: usize) -> Option<ChunkSummary> {
        let _ = i;
        None
    }

    /// The chunk that holds line `line`.
    pub fn chunk_of(line: LineNo) -> usize {
        usize::try_from(line / CHUNK_LINES as u64).unwrap_or(usize::MAX)
    }

    /// Add the line starts found in `bytes`, which begin at file offset `at` (must be the
    /// current [`LineIndex::indexed_bytes`]); `eof` says these are the last bytes of the file.
    pub fn extend(&mut self, at: u64, bytes: &[u8], eof: bool) {
        let _ = (at, bytes, eof);
    }

    /// Fill in a chunk's time and level summary.
    pub fn summarize(&mut self, chunk: usize, min_time: Option<Timestamp>, max_time: Option<Timestamp>, levels: LevelCounts) {
        let _ = (chunk, min_time, max_time, levels);
    }

    /// Forget everything from byte `offset` on (the file was truncated).
    pub fn truncate(&mut self, offset: u64) {
        let _ = offset;
    }

    /// Heap bytes the index uses (for the status bar's memory figure and the budget).
    pub fn memory_bytes(&self) -> u64 {
        0
    }
}
