//! Follow mode: watch a file for appends, truncation and rotation. Owned by the `index` stream.
//!
//! Uses OS file events (no polling, idle CPU 0 %), except on network filesystems, where it falls
//! back to a 1 s poll and says so ([`Watcher::is_polling`]).

use std::path::Path;

use crate::Result;

/// A running watch. Dropping it stops watching.
#[derive(Debug, Default)]
pub struct Watcher {
    polling: bool,
}

impl Watcher {
    /// Watch `path`; `on_change` runs on a watcher thread (it should only wake the UI, which then
    /// calls [`crate::LogFile::refresh`]).
    pub fn watch(path: &Path, on_change: impl Fn() + Send + Sync + 'static) -> Result<Watcher> {
        let _ = (path, on_change);
        Ok(Watcher::default())
    }

    /// Whether this watch polls (network filesystem) instead of getting events.
    pub fn is_polling(&self) -> bool {
        self.polling
    }
}
