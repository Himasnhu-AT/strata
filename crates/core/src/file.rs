//! An open log file: the map, the line index and the detected format. Owned by the `index`
//! stream.
//!
//! A `LogFile` is shared as `Arc<LogFile>` between the UI thread and jobs: indexing grows the
//! index while the table reads lines, so every method takes `&self` (interior locking).
//!
//! # Locks
//!
//! `indexing` (a mutex) serializes the writers: [`LogFile::index_more`] and [`LogFile::refresh`].
//! Readers take the index lock only to look up byte ranges, release it, then read the map under
//! its read lock. Lock order is always `indexing`, then `map`, then `index`, so nothing deadlocks.
//! The newline scan runs under the map's read lock but not the index lock, so the table keeps
//! reading lines while a step is indexed.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use same_file::Handle;

use crate::formats::{self, FieldNames};
use crate::index::{self, Found, IndexState, LineIndex};
use crate::limits::DETECT_BYTES;
use crate::mmap::{Mapped, RELEASE_EVERY};
use crate::{Error, FileId, FormatKind, LevelCounts, LineNo, Parsed, Result, Timestamp};

/// Bytes at the start of the file remembered to notice a file truncated and rewritten between
/// two refreshes (it may have grown past its old length).
const HEAD_BYTES: usize = 64;

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
    map: RwLock<Mapped>,
    index: RwLock<LineIndex>,
    state: Mutex<IndexState>,
    /// Held by the writers (`index_more`, `refresh`) for their whole run.
    indexing: Mutex<()>,
    /// The open file's identity (device + inode, or volume + file index), to notice rotation.
    identity: Option<Handle>,
    /// The first [`HEAD_BYTES`] bytes as indexed.
    head: Mutex<Vec<u8>>,
}

fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `head` (the first bytes of a file) looks binary: compressed, or mostly NULs or
/// invalid UTF-8. Latin-1 or a few stray bytes in a text log are fine.
pub fn looks_binary(head: &[u8]) -> bool {
    if head.is_empty() {
        return false;
    }
    if crate::gz::is_gzip(head) {
        return true;
    }
    let nul = memchr::memchr_iter(0, head).count();
    if nul * 10 > head.len() {
        return true;
    }
    let mut invalid = 0usize;
    for chunk in head.utf8_chunks() {
        invalid += chunk.invalid().len();
    }
    invalid * 2 > head.len()
}

impl LogFile {
    /// Open `path`: map it, detect its format from the first [`crate::limits::DETECT_BYTES`] and
    /// start with an empty index (fill it with [`LogFile::index_more`] from a job). Fails for a
    /// missing, unreadable or binary file.
    pub fn open(id: FileId, path: &Path) -> Result<LogFile> {
        let map = Mapped::open(path)?;
        let head_end = map.valid_len().min(DETECT_BYTES as u64);
        let head = map.get(0, head_end).unwrap_or_default();
        if looks_binary(head) {
            return Err(Error::Binary { path: path.to_path_buf() });
        }
        let format = formats::detect(head);
        let identity = map.file().and_then(|f| f.try_clone().ok()).and_then(|f| Handle::from_file(f).ok());
        Ok(LogFile {
            id,
            path: path.to_path_buf(),
            format,
            names: FieldNames::default(),
            map: RwLock::new(map),
            index: RwLock::new(LineIndex::new()),
            state: Mutex::new(IndexState::Idle),
            indexing: Mutex::new(()),
            identity,
            head: Mutex::new(Vec::new()),
        })
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size in bytes as last seen.
    pub fn len(&self) -> u64 {
        read(&self.map).len()
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
        read(&self.index).lines()
    }

    pub fn index_state(&self) -> IndexState {
        lock(&self.state).clone()
    }

    fn set_state(&self, s: IndexState) {
        *lock(&self.state) = s;
    }

    /// Index up to `max_bytes` more bytes; call it in a loop from a job, checking `cancel` in
    /// between. Returns the new state ([`IndexState::Complete`] when done).
    pub fn index_more(&self, max_bytes: u64) -> Result<IndexState> {
        let _writer = lock(&self.indexing);
        let map = read(&self.map);
        let limit = map.len().min(map.file_len()?);
        let (at, prev_cr) = {
            let ix = read(&self.index);
            (ix.indexed_bytes(), ix.ends_in_cr())
        };
        if at > limit {
            // The file shrank below what is indexed: `refresh` cuts the index first.
            self.set_state(IndexState::Complete);
            return Ok(IndexState::Complete);
        }
        let end = limit.min(at.saturating_add(max_bytes.max(1)));
        let eof = end >= limit;
        let bytes = map.get(at, end).unwrap_or_default();
        let mut found = Found::default();
        index::scan(at, bytes, prev_cr, &mut found);
        write(&self.index).apply(at, bytes.len() as u64, &found, eof);
        {
            let mut head = lock(&self.head);
            if head.len() < HEAD_BYTES {
                let want = (HEAD_BYTES as u64).min(end);
                *head = map.get(0, want).unwrap_or_default().to_vec();
            }
        }
        // Keep RSS near the index size: drop the scanned pages now and then.
        let release = at / RELEASE_EVERY != end / RELEASE_EVERY || (eof && end > RELEASE_EVERY && at < end);
        drop(map);
        if release {
            write(&self.map).release()?;
        }
        let state = if eof { IndexState::Complete } else { IndexState::Running { done: end, total: limit } };
        self.set_state(state.clone());
        Ok(state)
    }

    /// Index the rest of the file in steps of `step` bytes, calling `progress(done, total)` after
    /// each and checking `cancel` before each. Sets the state to `Cancelled` or `Failed` when it
    /// stops early (what is indexed stays usable).
    pub fn index_all(&self, step: u64, cancel: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<IndexState> {
        loop {
            if cancel() {
                self.set_state(IndexState::Cancelled);
                return Err(Error::Cancelled);
            }
            match self.index_more(step) {
                Ok(IndexState::Running { done, total }) => progress(done, total),
                Ok(s) => {
                    let n = read(&self.index).indexed_bytes();
                    progress(n, n);
                    return Ok(s);
                }
                Err(e) => {
                    self.set_state(IndexState::Failed(e.to_string()));
                    return Err(e);
                }
            }
        }
    }

    /// Read the index (summaries, ranges) under its lock. Keep `f` short.
    pub fn with_index<R>(&self, f: impl FnOnce(&LineIndex) -> R) -> R {
        f(&read(&self.index))
    }

    /// Line `n`, cut to `max_bytes`. `None` past the indexed part.
    pub fn line(&self, n: LineNo, max_bytes: usize) -> Option<Line> {
        self.lines_from(n, 1, max_bytes).pop()
    }

    /// Up to `count` lines from `from`, each cut to `max_bytes`.
    pub fn lines_from(&self, from: LineNo, count: usize, max_bytes: usize) -> Vec<Line> {
        let ranges: Vec<_> = {
            let ix = read(&self.index);
            let to = from.saturating_add(count as u64).min(ix.lines());
            (from..to).map_while(|n| ix.line_range(n).map(|r| (n, r))).collect()
        };
        if ranges.is_empty() {
            return Vec::new();
        }
        let map = read(&self.map);
        let valid = map.valid_len();
        let mut out = Vec::with_capacity(ranges.len());
        for (number, r) in ranges {
            let len = r.end - r.start;
            let take = len.min(max_bytes as u64);
            if r.start.saturating_add(take) > valid {
                break;
            }
            let Some(bytes) = map.get(r.start, r.start + take) else { break };
            out.push(Line { number, bytes: bytes.to_vec(), cut: take < len, len });
        }
        out
    }

    /// Run `f` on the raw bytes of lines `from..to` (searching without copies). `None` when the
    /// range is not indexed.
    ///
    /// The bytes include every line's newline. The file's map stays read-locked while `f` runs
    /// (a refresh waits for it), so call it per chunk, not for a whole 5 GB file at once.
    pub fn with_bytes<R>(&self, from: LineNo, to: LineNo, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        if from > to {
            return None;
        }
        let range = {
            let ix = read(&self.index);
            if to > ix.lines() {
                return None;
            }
            if from == to { None } else { Some(ix.raw_range(from)?.start..ix.raw_range(to - 1)?.end) }
        };
        let Some(range) = range else { return Some(f(&[])) };
        let map = read(&self.map);
        if range.end > map.valid_len() {
            return None;
        }
        map.get(range.start, range.end).map(f)
    }

    /// Fill in chunk `chunk`'s time span and level counts by parsing its lines (each cut to
    /// [`crate::limits::MAX_LINE_BYTES`]). For filters and the histogram, which skip chunks by
    /// their summary.
    pub fn summarize_chunk(&self, chunk: usize, cancel: &dyn Fn() -> bool) -> Result<()> {
        let Some(c) = self.with_index(|ix| ix.chunk(chunk)) else {
            return Err(Error::OutOfRange { what: format!("chunk {chunk}") });
        };
        let (mut min, mut max): (Option<Timestamp>, Option<Timestamp>) = (None, None);
        let mut levels = LevelCounts::default();
        let mut n = c.first_line;
        let end = c.first_line + u64::from(c.lines);
        while n < end {
            if cancel() {
                return Err(Error::Cancelled);
            }
            let batch = self.lines_from(n, 4096, crate::limits::MAX_LINE_BYTES);
            if batch.is_empty() {
                break;
            }
            for line in &batch {
                let p = self.parse(&line.bytes);
                if let Some(t) = p.time {
                    min = Some(min.map_or(t, |m| m.min(t)));
                    max = Some(max.map_or(t, |m| m.max(t)));
                }
                levels.add(p.level);
            }
            n += batch.len() as u64;
        }
        write(&self.index).summarize(chunk, min, max, levels);
        Ok(())
    }

    /// Check the file on disk (follow mode, after a watcher event) and update the map and index.
    pub fn refresh(&self) -> Result<FileChange> {
        let _writer = lock(&self.indexing);
        match Handle::from_path(&self.path) {
            Ok(h) => {
                if self.identity.as_ref().is_some_and(|id| *id != h) {
                    return Ok(FileChange::Replaced);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileChange::Removed),
            Err(e) => return Err(Error::io(&self.path, &e)),
        }
        let mut map = write(&self.map);
        let new_len = map.file_len()?;
        let old_len = map.len();
        let indexed = read(&self.index).indexed_bytes();
        let rewritten = {
            let head = lock(&self.head);
            let mut now = vec![0u8; head.len()];
            let n = if head.is_empty() { 0 } else { map.read_at(0, &mut now)? };
            n < head.len() || now != *head
        };
        if new_len < old_len || new_len < indexed || rewritten {
            let keep = if rewritten { 0 } else { new_len };
            write(&self.index).truncate(keep);
            lock(&self.head).truncate(keep.min(HEAD_BYTES as u64) as usize);
            let len = map.remap()?;
            let done = read(&self.index).indexed_bytes();
            self.set_state(IndexState::Running { done, total: len });
            return Ok(FileChange::Truncated);
        }
        if new_len > old_len {
            let len = map.remap()?;
            self.set_state(IndexState::Running { done: indexed, total: len });
            return Ok(FileChange::Appended { bytes: len.saturating_sub(old_len) });
        }
        Ok(FileChange::Unchanged)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use proptest::prelude::*;

    use super::*;

    fn file_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn index(lf: &LogFile) {
        assert_eq!(lf.index_all(7, &crate::never_cancel, &mut |_, _| {}).unwrap(), IndexState::Complete);
    }

    fn texts(lf: &LogFile) -> Vec<String> {
        lf.lines_from(0, usize::MAX, usize::MAX).iter().map(|l| l.text().into_owned()).collect()
    }

    #[test]
    fn empty_file() {
        let f = file_with(b"");
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        assert!(lf.is_empty());
        index(&lf);
        assert_eq!(lf.lines(), 0);
        assert_eq!(lf.line(0, 10), None);
        assert_eq!(lf.with_bytes(0, 0, <[u8]>::len), Some(0));
        assert_eq!(lf.with_bytes(0, 1, <[u8]>::len), None);
    }

    #[test]
    fn lines_crlf_and_cut() {
        let f = file_with(b"first\r\nsecond line\nlast");
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        assert_eq!(lf.index_state(), IndexState::Idle);
        index(&lf);
        assert_eq!(texts(&lf), ["first", "second line", "last"]);
        let l = lf.line(1, 6).unwrap();
        assert_eq!((l.text().as_ref(), l.cut, l.len, l.number), ("second", true, 11, 1));
        assert_eq!(lf.with_bytes(0, 2, |b| b.to_vec()), Some(b"first\r\nsecond line\n".to_vec()));
        assert_eq!(lf.with_bytes(2, 1, |_| ()), None);
        assert_eq!(lf.line(3, 10), None);
    }

    #[test]
    fn binary_and_missing() {
        let f = file_with(&[0u8; 1000]);
        assert!(matches!(LogFile::open(FileId(1), f.path()), Err(Error::Binary { .. })));
        let g = file_with(&[0x1f, 0x8b, 8, 0]);
        assert!(matches!(LogFile::open(FileId(1), g.path()), Err(Error::Binary { .. })));
        assert!(matches!(LogFile::open(FileId(1), Path::new("/no/such/file.log")), Err(Error::NotFound { .. })));
        assert!(!looks_binary("é latin\u{1} text \u{fffd}".as_bytes()));
        assert!(!looks_binary(b"caf\xe9 au lait\n"));
    }

    #[test]
    fn cancel_keeps_what_is_indexed() {
        let f = file_with("line\n".repeat(1000).as_bytes());
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert_eq!(lf.index_all(100, &cancel, &mut |_, _| {}), Err(Error::Cancelled));
        assert_eq!(lf.index_state(), IndexState::Cancelled);
        assert_eq!(lf.lines(), 60);
        assert_eq!(lf.line(59, 100).unwrap().text(), "line");
    }

    #[test]
    fn follow_append_truncate_rewrite() {
        let mut f = file_with(b"a\nb");
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        index(&lf);
        assert_eq!(texts(&lf), ["a", "b"]);
        assert_eq!(lf.refresh().unwrap(), FileChange::Unchanged);

        f.write_all(b"c\nd\n").unwrap();
        f.flush().unwrap();
        assert_eq!(lf.refresh().unwrap(), FileChange::Appended { bytes: 4 });
        assert!(matches!(lf.index_state(), IndexState::Running { .. }));
        index(&lf);
        assert_eq!(texts(&lf), ["a", "bc", "d"]);

        // Truncate in place (copytruncate) and write less than before. Windows refuses to
        // truncate a mapped file, which is the safe outcome there.
        if f.as_file().set_len(0).is_ok() {
            let mut w = std::fs::OpenOptions::new().write(true).open(f.path()).unwrap();
            w.write_all(b"x\n").unwrap();
            drop(w);
            assert_eq!(lf.refresh().unwrap(), FileChange::Truncated);
            index(&lf);
            assert_eq!(texts(&lf), ["x"]);

            // Truncate and rewrite past the old length between two refreshes.
            f.as_file().set_len(0).unwrap();
            let mut w = std::fs::OpenOptions::new().write(true).open(f.path()).unwrap();
            w.write_all(b"new content\nmore\n").unwrap();
            drop(w);
            assert_eq!(lf.refresh().unwrap(), FileChange::Truncated);
            index(&lf);
            assert_eq!(texts(&lf), ["new content", "more"]);
        }
    }

    #[test]
    fn follow_rotate_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, b"old\n").unwrap();
        let lf = LogFile::open(FileId(1), &path).unwrap();
        index(&lf);
        std::fs::rename(&path, dir.path().join("app.log.1")).unwrap();
        assert_eq!(lf.refresh().unwrap(), FileChange::Removed);
        std::fs::write(&path, b"new\n").unwrap();
        assert_eq!(lf.refresh().unwrap(), FileChange::Replaced);
        // The old file is still readable through the open handle.
        assert_eq!(texts(&lf), ["old"]);
    }

    #[test]
    fn long_line_is_never_loaded_whole() {
        let mut body = vec![b'x'; 8 << 20];
        body.push(b'\n');
        body.extend_from_slice(b"short\n");
        let f = file_with(&body);
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        index(&lf);
        let l = lf.line(0, 1024).unwrap();
        assert_eq!((l.bytes.len(), l.cut, l.len), (1024, true, 8 << 20));
        assert_eq!(lf.line(1, 1024).unwrap().text(), "short");
    }

    /// A 2 GB+ line with no newline (a sparse file: fast and takes no disk). Slow-ish (reads 2 GB
    /// of zero pages), so it runs only on request: `cargo test -p strata-core -- --ignored`.
    #[test]
    #[ignore]
    fn two_gigabyte_single_line() {
        let f = file_with(&[b'a'; DETECT_BYTES]);
        let len = (2u64 << 30) + 17;
        f.as_file().set_len(len).unwrap();
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        assert_eq!(lf.index_all(64 << 20, &crate::never_cancel, &mut |_, _| {}).unwrap(), IndexState::Complete);
        assert_eq!(lf.lines(), 1);
        let l = lf.line(0, 100).unwrap();
        assert_eq!((l.bytes.len(), l.cut, l.len), (100, true, len));
    }

    #[test]
    fn summarize_fills_chunk() {
        let f = file_with(b"one\ntwo\n");
        let lf = LogFile::open(FileId(1), f.path()).unwrap();
        index(&lf);
        lf.summarize_chunk(0, &crate::never_cancel).unwrap();
        assert!(lf.with_index(|ix| ix.chunk(0)).unwrap().summarized);
        assert!(lf.summarize_chunk(5, &crate::never_cancel).is_err());
    }

    #[test]
    fn shared_between_threads() {
        let f = file_with("some line of text\n".repeat(200_000).as_bytes());
        let lf = std::sync::Arc::new(LogFile::open(FileId(1), f.path()).unwrap());
        let job = {
            let lf = lf.clone();
            std::thread::spawn(move || lf.index_all(64 * 1024, &crate::never_cancel, &mut |_, _| {}))
        };
        while lf.index_state() != IndexState::Complete {
            let n = lf.lines();
            if n > 0 {
                assert_eq!(lf.line(n - 1, 100).unwrap().text(), "some line of text");
            }
            std::thread::yield_now();
        }
        assert_eq!(job.join().unwrap().unwrap(), IndexState::Complete);
        assert_eq!(lf.lines(), 200_000);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Any bytes: open either refuses (binary) or indexes exactly the reference lines, and
        /// no read panics.
        #[test]
        fn random_files_never_crash(bytes in proptest::collection::vec(any::<u8>(), 0..8192), step in 1u64..2000) {
            let f = file_with(&bytes);
            let Ok(lf) = LogFile::open(FileId(1), f.path()) else { return Ok(()) };
            let r = lf.index_all(step, &crate::never_cancel, &mut |_, _| {});
            prop_assert!(r.is_ok());
            let want: Vec<Vec<u8>> = {
                let mut v: Vec<Vec<u8>> = bytes.split(|b| *b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec()).collect();
                if bytes.last().is_none_or(|b| *b == b'\n') { v.pop(); }
                // The unterminated last line keeps a trailing `\r`.
                if let (Some(last), Some(b'\r')) = (v.last_mut(), bytes.last()) { last.push(b'\r'); }
                v
            };
            let got: Vec<Vec<u8>> = lf.lines_from(0, usize::MAX, usize::MAX).into_iter().map(|l| l.bytes).collect();
            prop_assert_eq!(got, want);
            let n = lf.lines();
            prop_assert!(lf.with_bytes(0, n, <[u8]>::len) == Some(bytes.len()));
            for k in 0..n.min(50) {
                prop_assert!(lf.line(k, 3).is_some());
            }
        }

        #[test]
        fn text_files_follow_growth(parts in proptest::collection::vec(proptest::collection::vec(prop_oneof![Just(b'\n'), Just(b'\r'), Just(b'z')], 0..300), 1..6)) {
            let mut f = file_with(b"");
            let lf = LogFile::open(FileId(1), f.path()).unwrap();
            let mut all = Vec::new();
            for p in &parts {
                f.write_all(p).unwrap();
                f.flush().unwrap();
                all.extend_from_slice(p);
                lf.refresh().unwrap();
                lf.index_all(37, &crate::never_cancel, &mut |_, _| {}).unwrap();
                let n = lf.lines();
                prop_assert!(lf.with_bytes(0, n, <[u8]>::len) == Some(all.len()));
            }
        }
    }
}
