//! The line index: where each line starts, in chunks of [`CHUNK_LINES`] lines, with per-chunk
//! summaries that let time filters, the histogram and "first error after T" skip whole chunks.
//! Owned by the `index` stream.
//!
//! The index is built incrementally by [`crate::LogFile::index_more`] and can be read while it
//! grows.
//!
//! # Encoding
//!
//! Every chunk stores its lines compactly, about 2.2 bytes a line for typical logs (lines of
//! 64–8 191 bytes), so a 50 M-line file needs about 110 MB:
//!
//! - each line as one LEB128 varint: `len << 1 | cr`, where `len` is the line's length including
//!   its `\n` and `cr` says it ends in `\r\n`;
//! - every [`GROUP`] lines a mark: the absolute start offset of that line (`u64`) and where its
//!   varint is (`u32`), 0.19 bytes a line.
//!
//! Finding line `n` is one division (its chunk), one array read (its mark) and at most
//! [`GROUP`]` - 1` varint decodes, well under a microsecond. Lines have no length limit (a 2 GB
//! line is one varint); a chunk's varints are at most 10 bytes × [`CHUNK_LINES`], so `u32`
//! positions always fit.

use std::ops::Range;

use crate::limits::CHUNK_LINES;
use crate::{LevelCounts, LineNo, Timestamp};

/// Lines between two random-access marks inside a chunk.
pub const GROUP: usize = 64;

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

/// One chunk: up to [`CHUNK_LINES`] lines.
#[derive(Clone, Debug, Default)]
struct Chunk {
    first_line: u64,
    /// Where the chunk's first line starts.
    start: u64,
    /// Where its last line ends (after the newline).
    end: u64,
    lines: u32,
    /// One varint per line: `len << 1 | cr`.
    varints: Vec<u8>,
    /// Start offset of line `g * GROUP` (chunk-relative numbering).
    mark_off: Vec<u64>,
    /// Position in `varints` of line `g * GROUP`.
    mark_pos: Vec<u32>,
    min_time: Option<Timestamp>,
    max_time: Option<Timestamp>,
    levels: LevelCounts,
    summarized: bool,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Decode the varint at `*pos` and advance; `None` on malformed or missing bytes.
fn get_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *data.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f).checked_shl(shift)?;
        if b < 0x80 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

impl Chunk {
    fn new(first_line: u64, start: u64) -> Chunk {
        Chunk { first_line, start, end: start, ..Chunk::default() }
    }

    #[inline]
    fn push(&mut self, end: u64, cr: bool) {
        if (self.lines as usize).is_multiple_of(GROUP) {
            self.mark_off.push(self.end);
            self.mark_pos.push(self.varints.len() as u32);
        }
        let len = end.saturating_sub(self.end);
        put_varint(&mut self.varints, len << 1 | u64::from(cr));
        self.end = end;
        self.lines += 1;
    }

    /// Line `j` of this chunk: (start, end after the newline, ends in `\r\n`, varint position).
    fn line(&self, j: u32) -> Option<(u64, u64, bool, usize)> {
        if j >= self.lines {
            return None;
        }
        let g = j as usize / GROUP;
        let mut off = *self.mark_off.get(g)?;
        let mut pos = *self.mark_pos.get(g)? as usize;
        let mut k = (g * GROUP) as u32;
        loop {
            let at = pos;
            let v = get_varint(&self.varints, &mut pos)?;
            let end = off.checked_add(v >> 1)?;
            if k == j {
                return Some((off, end, v & 1 == 1, at));
            }
            off = end;
            k += 1;
        }
    }

    /// The chunk-relative line that contains `offset` (`start <= offset < end`).
    fn line_at_offset(&self, offset: u64) -> Option<u32> {
        if offset < self.start || offset >= self.end {
            return None;
        }
        let g = self.mark_off.partition_point(|&m| m <= offset).checked_sub(1)?;
        let mut off = *self.mark_off.get(g)?;
        let mut pos = *self.mark_pos.get(g)? as usize;
        let mut k = (g * GROUP) as u32;
        while k < self.lines {
            let v = get_varint(&self.varints, &mut pos)?;
            off = off.checked_add(v >> 1)?;
            if offset < off {
                return Some(k);
            }
            k += 1;
        }
        None
    }

    /// Keep the first `n` lines (`0 < n <= lines`).
    fn truncate_lines(&mut self, n: u32) {
        if n >= self.lines {
            return;
        }
        if let Some((start, _, _, pos)) = self.line(n) {
            self.varints.truncate(pos);
            self.end = start;
            self.lines = n;
            let marks = (n as usize).div_ceil(GROUP);
            self.mark_off.truncate(marks);
            self.mark_pos.truncate(marks);
            self.clear_summary();
        }
    }

    fn clear_summary(&mut self) {
        self.min_time = None;
        self.max_time = None;
        self.levels = LevelCounts::default();
        self.summarized = false;
    }

    fn seal(&mut self) {
        self.varints.shrink_to_fit();
        self.mark_off.shrink_to_fit();
        self.mark_pos.shrink_to_fit();
    }

    fn memory_bytes(&self) -> u64 {
        (self.varints.capacity() + self.mark_off.capacity() * 8 + self.mark_pos.capacity() * 4) as u64
    }
}

/// Newlines found in a run of bytes, ready to add to a [`LineIndex`] (see [`scan`]).
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// Line ends (offset after the `\n`), each `end << 1 | cr`.
    ends: Vec<u64>,
    /// Whether the last byte scanned is `\r`.
    last_cr: bool,
}

/// Find the newlines in `bytes`, which start at file offset `at`; `prev_cr` says the byte before
/// `at` is `\r`. Runs without the index lock, so readers are not blocked while it scans.
pub fn scan(at: u64, bytes: &[u8], prev_cr: bool, found: &mut Found) {
    found.ends.clear();
    for p in memchr::memchr_iter(b'\n', bytes) {
        let cr = match p.checked_sub(1) {
            Some(q) => bytes.get(q) == Some(&b'\r'),
            None => prev_cr,
        };
        found.ends.push((at + p as u64 + 1) << 1 | u64::from(cr));
    }
    found.last_cr = match bytes.last() {
        Some(b) => *b == b'\r',
        None => prev_cr,
    };
}

/// Line start offsets for one file.
#[derive(Clone, Debug, Default)]
pub struct LineIndex {
    chunks: Vec<Chunk>,
    lines: u64,
    /// Bytes looked at (the index covers them; past the last newline they belong to the pending
    /// line, which is counted only at the end of the file).
    scanned: u64,
    /// The byte before `scanned` is `\r`.
    last_cr: bool,
    /// The last line has no newline (counted because the file ended there).
    tail_open: bool,
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
        self.scanned
    }

    /// Whether the byte before [`LineIndex::indexed_bytes`] is `\r` (the `prev_cr` of [`scan`]).
    pub fn ends_in_cr(&self) -> bool {
        self.last_cr
    }

    /// Where the pending (not yet terminated) line starts: the end of the last complete line.
    fn pending_start(&self) -> u64 {
        self.chunks.last().map_or(0, |c| c.end)
    }

    /// (start, end after the newline, ends in `\r\n`) of `line`.
    fn locate(&self, line: LineNo) -> Option<(u64, u64, bool)> {
        let c = self.chunks.get(LineIndex::chunk_of(line))?;
        let j = u32::try_from(line.checked_sub(c.first_line)?).ok()?;
        c.line(j).map(|(s, e, cr, _)| (s, e, cr))
    }

    /// Byte range of line `line`, without its `\n` (or `\r\n`). `None` past the indexed part.
    pub fn line_range(&self, line: LineNo) -> Option<Range<u64>> {
        let (start, end, cr) = self.locate(line)?;
        let open = self.tail_open && line.checked_add(1) == Some(self.lines);
        let cut = if open { 0 } else { 1 + u64::from(cr) };
        Some(start..end.saturating_sub(cut).max(start))
    }

    /// Byte range of line `line` including its newline (the raw bytes searched).
    pub fn raw_range(&self, line: LineNo) -> Option<Range<u64>> {
        let (start, end, _) = self.locate(line)?;
        Some(start..end)
    }

    /// The line that contains byte `offset`.
    pub fn line_at_offset(&self, offset: u64) -> Option<LineNo> {
        let i = self.chunks.partition_point(|c| c.start <= offset).checked_sub(1)?;
        let c = self.chunks.get(i)?;
        c.line_at_offset(offset).map(|j| c.first_line + u64::from(j))
    }

    /// Number of chunks (the last may be partial).
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }

    /// The summary of chunk `i`.
    pub fn chunk(&self, i: usize) -> Option<ChunkSummary> {
        let c = self.chunks.get(i)?;
        Some(ChunkSummary {
            first_line: c.first_line,
            lines: c.lines,
            bytes: c.start..c.end,
            min_time: c.min_time,
            max_time: c.max_time,
            levels: c.levels,
            summarized: c.summarized,
        })
    }

    /// The chunk that holds line `line`.
    pub fn chunk_of(line: LineNo) -> usize {
        usize::try_from(line / CHUNK_LINES as u64).unwrap_or(usize::MAX)
    }

    /// Add the line starts found in `bytes`, which begin at file offset `at` (must be the
    /// current [`LineIndex::indexed_bytes`]); `eof` says these are the last bytes of the file.
    pub fn extend(&mut self, at: u64, bytes: &[u8], eof: bool) {
        let mut found = Found::default();
        scan(at, bytes, self.last_cr, &mut found);
        self.apply(at, bytes.len() as u64, &found, eof);
    }

    /// Add what [`scan`] found in `len` bytes at `at` (must be [`LineIndex::indexed_bytes`];
    /// anything else is ignored).
    pub fn apply(&mut self, at: u64, len: u64, found: &Found, eof: bool) {
        if at != self.scanned {
            return;
        }
        if self.tail_open && len > 0 {
            // The unterminated last line goes on: index it again when its end is found.
            self.truncate_lines(self.lines.saturating_sub(1));
            self.tail_open = false;
        }
        for &e in &found.ends {
            self.push_line(e >> 1, e & 1 == 1);
        }
        self.scanned = at.saturating_add(len);
        if len > 0 {
            self.last_cr = found.last_cr;
        }
        if eof && !self.tail_open && self.pending_start() < self.scanned {
            self.push_line(self.scanned, false);
            self.tail_open = true;
        }
    }

    #[inline]
    fn push_line(&mut self, end: u64, cr: bool) {
        let full = self.chunks.last().is_none_or(|c| c.lines as usize >= CHUNK_LINES);
        if full {
            let start = self.pending_start();
            if let Some(c) = self.chunks.last_mut() {
                c.seal();
            }
            self.chunks.push(Chunk::new(self.lines, start));
        }
        if let Some(c) = self.chunks.last_mut() {
            c.push(end, cr);
            self.lines += 1;
        }
    }

    /// Keep the first `n` lines; the scan restarts after them.
    fn truncate_lines(&mut self, n: u64) {
        if n >= self.lines {
            return;
        }
        let keep_chunks = usize::try_from(n.div_ceil(CHUNK_LINES as u64)).unwrap_or(usize::MAX);
        self.chunks.truncate(keep_chunks);
        if let Some(c) = self.chunks.last_mut() {
            let j = u32::try_from(n - c.first_line).unwrap_or(u32::MAX);
            c.truncate_lines(j);
        }
        self.lines = n;
        self.scanned = self.pending_start();
        self.last_cr = false;
        self.tail_open = false;
    }

    /// Fill in a chunk's time and level summary.
    pub fn summarize(&mut self, chunk: usize, min_time: Option<Timestamp>, max_time: Option<Timestamp>, levels: LevelCounts) {
        if let Some(c) = self.chunks.get_mut(chunk) {
            c.min_time = min_time;
            c.max_time = max_time;
            c.levels = levels;
            c.summarized = true;
        }
    }

    /// Forget everything from byte `offset` on (the file was truncated).
    pub fn truncate(&mut self, offset: u64) {
        if offset >= self.scanned {
            return;
        }
        if offset >= self.pending_start() {
            // Only pending bytes (no newline among them) go; every complete line stays. (An open
            // tail line ends at `scanned`, so it is never in this case.)
            self.scanned = self.pending_start();
            self.last_cr = false;
            return;
        }
        let keep = self.line_at_offset(offset).unwrap_or(0);
        self.truncate_lines(keep);
    }

    /// Heap bytes the index uses (for the status bar's memory figure and the budget).
    pub fn memory_bytes(&self) -> u64 {
        let own = (self.chunks.capacity() * std::mem::size_of::<Chunk>()) as u64;
        own + self.chunks.iter().map(Chunk::memory_bytes).sum::<u64>()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The obvious index: every line's content range, split on `\n`, `\r\n` stripped.
    fn reference(bytes: &[u8]) -> Vec<Range<u64>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                let end = if i > start && bytes[i - 1] == b'\r' { i - 1 } else { i };
                out.push(start as u64..end as u64);
                start = i + 1;
            }
        }
        if start < bytes.len() {
            out.push(start as u64..bytes.len() as u64);
        }
        out
    }

    fn build(bytes: &[u8], step: usize) -> LineIndex {
        let mut ix = LineIndex::new();
        let mut at = 0;
        loop {
            let end = (at + step.max(1)).min(bytes.len());
            ix.extend(at as u64, &bytes[at..end], end == bytes.len());
            at = end;
            if at == bytes.len() {
                return ix;
            }
        }
    }

    fn check(ix: &LineIndex, bytes: &[u8]) {
        let want = reference(bytes);
        assert_eq!(ix.lines(), want.len() as u64);
        assert_eq!(ix.indexed_bytes(), bytes.len() as u64);
        for (n, r) in want.iter().enumerate() {
            assert_eq!(ix.line_range(n as u64).as_ref(), Some(r), "line {n}");
            assert_eq!(ix.line_at_offset(r.start), Some(n as u64));
        }
        assert_eq!(ix.line_range(want.len() as u64), None);
        assert_eq!(ix.line_at_offset(bytes.len() as u64), None);
    }

    #[test]
    fn empty_and_simple() {
        let ix = build(b"", 10);
        assert_eq!(ix.lines(), 0);
        assert_eq!(ix.line_range(0), None);
        assert_eq!(ix.chunks(), 0);
        for text in [&b"a\nbb\r\nccc"[..], b"\n\n\n", b"no newline", b"\r\n\r", b"x\n"] {
            for step in [1, 2, 3, 100] {
                check(&build(text, step), text);
            }
        }
    }

    #[test]
    fn chunks_and_summaries() {
        let text = "l\n".repeat(CHUNK_LINES * 2 + 5);
        let mut ix = build(text.as_bytes(), 7777);
        check(&ix, text.as_bytes());
        assert_eq!(ix.chunks(), 3);
        let c1 = ix.chunk(1).unwrap();
        assert_eq!(c1.first_line, CHUNK_LINES as u64);
        assert_eq!(c1.lines as usize, CHUNK_LINES);
        assert_eq!(c1.bytes, (CHUNK_LINES as u64 * 2)..(CHUNK_LINES as u64 * 4));
        assert!(!c1.summarized);
        ix.summarize(1, Some(Timestamp(1)), Some(Timestamp(2)), LevelCounts::default());
        assert!(ix.chunk(1).unwrap().summarized);
        assert_eq!(ix.chunk(3), None);
        assert!(ix.memory_bytes() > 0);
        // About 1 byte per 2-byte line plus marks.
        assert!(ix.memory_bytes() < ix.lines() * 2, "{} bytes for {} lines", ix.memory_bytes(), ix.lines());
    }

    #[test]
    fn tail_line_grows() {
        let mut ix = LineIndex::new();
        ix.extend(0, b"ab\ncd", true);
        assert_eq!(ix.lines(), 2);
        assert_eq!(ix.line_range(1), Some(3..5));
        ix.extend(5, b"ef\r\ngh\n", false);
        assert_eq!(ix.lines(), 3);
        assert_eq!(ix.line_range(1), Some(3..7));
        assert_eq!(ix.line_range(2), Some(9..11));
    }

    #[test]
    fn huge_line_is_one_entry() {
        let mut ix = LineIndex::new();
        let len = 3u64 << 30;
        let mut found = Found::default();
        found.ends.push((len + 1) << 1);
        ix.apply(0, len + 1, &found, true);
        assert_eq!(ix.line_range(0), Some(0..len));
        assert_eq!(ix.line_at_offset(len - 1), Some(0));
    }

    #[test]
    fn truncate_then_regrow() {
        let text = b"one\ntwo\nthree\nfour";
        let mut ix = build(text, 5);
        ix.truncate(9);
        assert_eq!(ix.lines(), 2);
        assert_eq!(ix.indexed_bytes(), 8);
        ix.extend(8, b"THREE\n", true);
        check(&ix, b"one\ntwo\nTHREE\n");
        ix.truncate(0);
        assert_eq!(ix.lines(), 0);
        assert_eq!(ix.indexed_bytes(), 0);
        assert_eq!(ix.chunks(), 0);
    }

    #[test]
    fn wrong_offset_is_ignored() {
        let mut ix = build(b"a\nb\n", 4);
        ix.extend(99, b"zzz\n", true);
        check(&ix, b"a\nb\n");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn random_bytes_match_reference(bytes in proptest::collection::vec(any::<u8>(), 0..4096), step in 1usize..600) {
            check(&build(&bytes, step), &bytes);
        }

    }

    proptest! {
        // Big inputs (many marks and chunks of lines): fewer cases.
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn newline_heavy_match_reference(
            bytes in proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'\r'), Just(b'a')], 0..6000),
            step in 1usize..3000,
        ) {
            check(&build(&bytes, step), &bytes);
        }

        #[test]
        fn truncate_equals_fresh(bytes in proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'\r'), any::<u8>()], 0..3000), cut in 0usize..3000, step in 1usize..300) {
            let cut = cut.min(bytes.len());
            let mut ix = build(&bytes, step);
            ix.truncate(cut as u64);
            let at = ix.indexed_bytes() as usize;
            prop_assert!(at <= cut);
            ix.extend(at as u64, &bytes[at..cut], true);
            check(&ix, &bytes[..cut]);
        }
    }
}
