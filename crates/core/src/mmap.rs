//! Read-only memory maps of log files. Owned by the `index` stream.
//!
//! The only module allowed `unsafe` (`#![allow(unsafe_code)]` here, a `// SAFETY:` comment on every
//! block). Everything outside reads bytes through [`Mapped::get`] and never holds a slice across a
//! [`Mapped::remap`].

use std::path::{Path, PathBuf};

use crate::Result;

/// A file mapped read-only. Opened with share flags that let other processes keep writing it
/// (Windows: `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`).
#[derive(Debug, Default)]
pub struct Mapped {
    path: PathBuf,
    len: u64,
}

impl Mapped {
    /// Map `path` (an empty file maps to no bytes).
    pub fn open(path: &Path) -> Result<Mapped> {
        Ok(Mapped { path: path.to_path_buf(), len: 0 })
    }

    /// The mapped path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mapped length in bytes (the file may have grown since).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether nothing is mapped.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bytes in `start..end`, or `None` when the range is outside the mapping.
    pub fn get(&self, start: u64, end: u64) -> Option<&[u8]> {
        let _ = (start, end);
        None
    }

    /// Map the file again at its current length (after it grew). Returns the new length.
    pub fn remap(&mut self) -> Result<u64> {
        Ok(self.len)
    }
}
