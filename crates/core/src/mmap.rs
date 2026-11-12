//! Read-only memory maps of log files. Owned by the `index` stream.
//!
//! The only module allowed `unsafe` (`#![allow(unsafe_code)]` here, a `// SAFETY:` comment on every
//! block). Everything outside reads bytes through [`Mapped::get`] and never holds a slice across a
//! [`Mapped::remap`] (the borrow checker enforces it: `get` borrows `&self`, `remap` needs
//! `&mut self`; [`crate::LogFile`] keeps its `Mapped` in a `RwLock`).
//!
//! # A file that shrinks while mapped
//!
//! Reading a mapped page that lies past the end of a file another process truncated raises
//! `SIGBUS` on macOS and Linux, which would kill the app. We keep the map but never touch a byte
//! the file no longer has:
//!
//! - every read in [`crate::LogFile`] (a line, a run of lines, a search range, an index step) first
//!   asks the OS for the file's current length ([`Mapped::valid_len`], one `fstat`, about a
//!   microsecond) and reads only below it;
//! - [`crate::LogFile::refresh`] (follow mode) remaps at the new length and drops the index past it.
//!
//! What remains is the race between that `fstat` and the read (microseconds, or as long as `f`
//! runs inside [`crate::LogFile::with_bytes`]), and only against a writer that truncates in place
//! (`copytruncate`); rotation by rename, the usual case, keeps the old inode and is always safe.
//! We accept that window rather than install a process-wide `SIGBUS` handler (Rust's runtime and
//! ashlar's crash guard own signal handling). Reading through `read()` instead of a map would close
//! it, at the cost of the zero-copy search and the 1 GB/s index.
//!
//! On Windows the OS refuses to truncate a file while a view of it is mapped (the writer gets
//! `ERROR_USER_MAPPED_FILE`), so no fault is possible there; appends work. `std::fs::File::open`
//! opens with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`, so writers, renames and
//! deletes by other processes are not blocked.
#![allow(unsafe_code)]

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::{Mmap, MmapOptions};

use crate::{Error, Result};

/// Bytes the index job scans between two [`Mapped::release`] calls.
pub const RELEASE_EVERY: u64 = 64 << 20;

/// A file mapped read-only. Opened with share flags that let other processes keep writing it
/// (Windows: `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`).
#[derive(Debug, Default)]
pub struct Mapped {
    path: PathBuf,
    file: Option<File>,
    map: Option<Mmap>,
    len: u64,
}

impl Mapped {
    /// Map `path` (an empty file maps to no bytes).
    pub fn open(path: &Path) -> Result<Mapped> {
        let file = File::open(path).map_err(|e| Error::io(path, &e))?;
        let meta = file.metadata().map_err(|e| Error::io(path, &e))?;
        if meta.is_dir() {
            return Err(Error::Io { path: path.to_path_buf(), message: "is a directory".into() });
        }
        let mut m = Mapped { path: path.to_path_buf(), file: Some(file), map: None, len: 0 };
        m.map_at(meta.len())?;
        Ok(m)
    }

    /// Replace the map with one of `len` bytes.
    fn map_at(&mut self, len: u64) -> Result<()> {
        self.map = None;
        self.len = 0;
        if len == 0 {
            return Ok(());
        }
        let Some(file) = self.file.as_ref() else { return Ok(()) };
        let n = usize::try_from(len).map_err(|_| Error::TooLarge { path: self.path.clone(), limit: usize::MAX as u64 })?;
        // SAFETY: mapping is unsafe because the bytes can change or vanish under us when another
        // process writes or truncates the file. Changing bytes are fine: they are untrusted input
        // everywhere and nothing relies on two reads agreeing. Vanished bytes (truncation) are
        // never read: every caller clamps to `valid_len()` first (the module docs describe the
        // remaining race and why it is accepted). The map is read-only and owned by `self`, and
        // `get` ties every slice to `&self`, so no slice outlives it or a `remap`.
        let map = unsafe { MmapOptions::new().len(n).map(file) }.map_err(|e| Error::io(&self.path, &e))?;
        self.map = Some(map);
        self.len = len;
        Ok(())
    }

    /// The mapped path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The open file (for identity checks).
    pub fn file(&self) -> Option<&File> {
        self.file.as_ref()
    }

    /// Mapped length in bytes (the file may have grown since).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether nothing is mapped.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The file's length on disk now (one `fstat`).
    pub fn file_len(&self) -> Result<u64> {
        match &self.file {
            Some(f) => f.metadata().map(|m| m.len()).map_err(|e| Error::io(&self.path, &e)),
            None => Ok(0),
        }
    }

    /// The bytes that are safe to read now: the mapped length, or less if the file shrank since.
    pub fn valid_len(&self) -> u64 {
        self.len.min(self.file_len().unwrap_or(0))
    }

    /// The bytes in `start..end`, or `None` when the range is outside the mapping.
    ///
    /// Does not look at the file's current length: callers that may race a truncation clamp `end`
    /// to [`Mapped::valid_len`] first.
    pub fn get(&self, start: u64, end: u64) -> Option<&[u8]> {
        if start > end || end > self.len {
            return None;
        }
        if start == end {
            return Some(&[]);
        }
        let map = self.map.as_ref()?;
        map.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
    }

    /// Map the same length again, which drops every page the old map had resident (they come
    /// back from the page cache when touched). The index job calls it every
    /// [`RELEASE_EVERY`] bytes, so a fully indexed 5 GB file does not count 5 GB of mapped
    /// pages in the process's RSS. (`madvise(MADV_DONTNEED)` would do it on Linux only; macOS
    /// keeps the pages, and Windows has no equivalent for a view.)
    pub fn release(&mut self) -> Result<()> {
        if self.map.is_some() {
            let len = self.len;
            self.map_at(len)?;
        }
        Ok(())
    }

    /// Read up to `buf.len()` bytes at `offset` with a positioned read (not through the map, so a
    /// shrunk file is never touched). Returns the number of bytes read.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let Some(file) = &self.file else { return Ok(0) };
        #[cfg(unix)]
        let r = std::os::unix::fs::FileExt::read_at(file, buf, offset);
        #[cfg(windows)]
        let r = std::os::windows::fs::FileExt::seek_read(file, buf, offset);
        #[cfg(not(any(unix, windows)))]
        let r: std::io::Result<usize> = {
            let _ = (file, buf, offset);
            Ok(0)
        };
        r.map_err(|e| Error::io(&self.path, &e))
    }

    /// Map the file again at its current length (after it grew or shrank). Returns the new length.
    pub fn remap(&mut self) -> Result<u64> {
        let len = self.file_len()?;
        if len != self.len {
            self.map_at(len)?;
        }
        Ok(self.len)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn maps_grows_and_bounds() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut m = Mapped::open(f.path()).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.get(0, 0), Some(&[][..]));
        assert_eq!(m.get(0, 1), None);
        f.write_all(b"hello\n").unwrap();
        f.flush().unwrap();
        assert_eq!(m.remap().unwrap(), 6);
        assert_eq!(m.get(0, 5), Some(&b"hello"[..]));
        assert_eq!(m.get(5, 4), None);
        assert_eq!(m.get(0, 7), None);
        assert_eq!(m.get(u64::MAX, u64::MAX), None);
        let mut buf = [0u8; 3];
        assert_eq!(m.read_at(1, &mut buf).unwrap(), 3);
        assert_eq!(&buf, b"ell");
    }

    #[test]
    fn shrink_is_seen_before_reading() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&[b'x'; 10_000]).unwrap();
        f.flush().unwrap();
        let mut m = Mapped::open(f.path()).unwrap();
        assert_eq!(m.len(), 10_000);
        // Windows refuses to truncate a mapped file; that is the safe outcome there.
        if f.as_file().set_len(10).is_ok() {
            assert_eq!(m.valid_len(), 10);
            assert_eq!(m.remap().unwrap(), 10);
            assert_eq!(m.len(), 10);
        }
    }

    #[test]
    fn missing_and_directory() {
        assert!(matches!(Mapped::open(Path::new("/definitely/not/here.log")), Err(Error::NotFound { .. })));
        let d = tempfile::tempdir().unwrap();
        assert!(Mapped::open(d.path()).is_err());
    }
}
