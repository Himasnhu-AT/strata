//! Follow mode: watch a file for appends, truncation and rotation. Owned by the `index` stream.
//!
//! Uses OS file events (no polling, idle CPU 0 %), except on network filesystems, where it falls
//! back to a 1 s poll and says so ([`Watcher::is_polling`]).
//!
//! The callback only says "look again": [`crate::LogFile::refresh`] works out what changed
//! (append, truncate, replace, remove) from the file itself, so a coalesced or extra event costs
//! nothing but a `stat`. Per platform (the `notify` crate):
//!
//! - **Linux**: inotify on the file's directory (non-recursive), filtered to the file's name. Sees
//!   every write, truncation, rename and create, so rotation is caught.
//! - **macOS and the BSDs**: kqueue on the file (every write and truncation, rename and delete of
//!   the open file) and on its directory (a new file created under the name after a rotation).
//!   FSEvents is not used: it reports a write only when the writer closes the file, and a logger
//!   keeps it open. kqueue cannot say which entry of a directory changed, so directory changes
//!   always wake us (rare: creates, renames and deletes, not writes).
//! - **Windows**: `ReadDirectoryChangesW` on the directory, filtered to the name, plus a 1 s
//!   `stat` of the file as a backstop, because NTFS may defer the size update of a directory entry
//!   until the writer closes or flushes its handle. One `stat` a second is not measurable CPU.
//!
//! Open, read and attribute-only events are ignored, so our own reads (or an `atime` update) never
//! wake us in a loop.
//!
//! A path counts as remote (polled) when: on Linux, `statfs` reports NFS, SMB/CIFS, AFS, Coda,
//! 9P or FUSE (sshfs and friends); on macOS and the BSDs, the mount lacks `MNT_LOCAL`; on
//! Windows, the path is a UNC path (`\\server\share`; a mapped drive letter is not detected and
//! gets events plus the backstop poll). A watch the OS refuses (out of inotify watches, for
//! example) also falls back to polling.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::{Error, Result};

/// How often a polled watch looks at the file.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Whether the event backend is kqueue, which cannot name the directory entry that changed.
const KQUEUE: bool =
    cfg!(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"));

/// A running watch. Dropping it stops watching.
#[derive(Default)]
pub struct Watcher {
    polling: bool,
    inner: Vec<Box<dyn notify::Watcher + Send>>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").field("polling", &self.polling).field("watches", &self.inner.len()).finish()
    }
}

fn notify_err(path: &Path, e: &notify::Error) -> Error {
    Error::Io { path: path.to_path_buf(), message: e.to_string() }
}

/// Whether an event kind can mean the file's contents or identity changed.
fn relevant(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        _ => true,
    }
}

fn poll_watch(path: &Path, dir: &Path, handler: impl notify::EventHandler) -> Result<PollWatcher> {
    let config = Config::default().with_poll_interval(POLL_INTERVAL).with_compare_contents(false);
    let mut w = PollWatcher::new(handler, config).map_err(|e| notify_err(dir, &e))?;
    // Polling the file alone is cheaper than its whole directory; the directory is the fallback
    // when the file is missing right now (between a rotation's rename and create).
    if w.watch(path, RecursiveMode::NonRecursive).is_err() {
        w.watch(dir, RecursiveMode::NonRecursive).map_err(|e| notify_err(dir, &e))?;
    }
    Ok(w)
}

impl Watcher {
    /// Watch `path`; `on_change` runs on a watcher thread (it should only wake the UI, which then
    /// calls [`crate::LogFile::refresh`]).
    pub fn watch(path: &Path, on_change: impl Fn() + Send + Sync + 'static) -> Result<Watcher> {
        let path = std::path::absolute(path).map_err(|e| Error::io(path, &e))?;
        let Some(name) = path.file_name().map(OsString::from) else {
            return Err(Error::Io { path, message: "not a file path".into() });
        };
        let dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        // Event backends report real paths (macOS temp dirs sit under the `/var` symlink).
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let path = dir.join(&name);
        let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(on_change);
        let handler = move |res: notify::Result<Event>| match res {
            Ok(ev) => {
                let ours = KQUEUE || ev.need_rescan() || ev.paths.iter().any(|p| p.file_name() == Some(name.as_os_str()));
                if ours && relevant(&ev.kind) {
                    on_change();
                }
            }
            Err(_) => on_change(),
        };

        if !is_remote(&dir)
            && let Ok(mut w) = RecommendedWatcher::new(handler.clone(), Config::default())
            && w.watch(&dir, RecursiveMode::NonRecursive).is_ok()
        {
            if KQUEUE {
                // The file itself: kqueue reports its writes only on a watch of its own. (It
                // may be missing between a rotation's rename and create; the directory watch
                // still sees the create.)
                let _ = w.watch(&path, RecursiveMode::NonRecursive);
            }
            let mut inner: Vec<Box<dyn notify::Watcher + Send>> = vec![Box::new(w)];
            if cfg!(windows)
                && let Ok(p) = poll_watch(&path, &dir, handler)
            {
                inner.push(Box::new(p));
            }
            return Ok(Watcher { polling: false, inner });
        }
        let w = poll_watch(&path, &dir, handler)?;
        Ok(Watcher { polling: true, inner: vec![Box::new(w)] })
    }

    /// Whether this watch polls (network filesystem) instead of getting events.
    pub fn is_polling(&self) -> bool {
        self.polling
    }
}

/// Whether `dir` is on a network filesystem (no reliable file events there).
pub fn is_remote(dir: &Path) -> bool {
    remote_impl(dir)
}

#[cfg(target_os = "linux")]
fn remote_impl(dir: &Path) -> bool {
    // statfs(2) f_type magic numbers (linux/magic.h).
    const REMOTE: [u64; 9] = [
        0x6969,      // NFS
        0x517b,      // SMB
        0xff53_4d42, // CIFS
        0xfe53_4d42, // SMB2
        0x5346_414f, // AFS
        0x6b41_4653, // kAFS
        0x7375_7245, // Coda
        0x0102_1997, // 9P
        0x6573_5546, // FUSE (sshfs, rclone, ...)
    ];
    match rustix::fs::statfs(dir) {
        #[allow(clippy::unnecessary_cast)]
        Ok(s) => REMOTE.contains(&(s.f_type as u64)),
        Err(_) => false,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn remote_impl(dir: &Path) -> bool {
    // `MNT_LOCAL` in statfs f_flags (the same value on macOS and the BSDs).
    const MNT_LOCAL: u64 = 0x0000_1000;
    match rustix::fs::statfs(dir) {
        #[allow(clippy::unnecessary_cast)]
        Ok(s) => (s.f_flags as u64) & MNT_LOCAL == 0,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn remote_impl(dir: &Path) -> bool {
    let s = dir.as_os_str().to_string_lossy();
    (s.starts_with(r"\\") && !s.starts_with(r"\\?\") && !s.starts_with(r"\\.\")) || s.starts_with(r"\\?\UNC\")
}

#[cfg(not(any(unix, windows)))]
fn remote_impl(_dir: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::mpsc;

    use super::*;

    fn wait(rx: &mpsc::Receiver<()>) -> bool {
        let got = rx.recv_timeout(Duration::from_secs(10)).is_ok();
        std::thread::sleep(Duration::from_millis(100));
        while rx.try_recv().is_ok() {}
        got
    }

    #[test]
    fn local_temp_dir_is_not_remote() {
        let d = tempfile::tempdir().unwrap();
        assert!(!is_remote(d.path()));
    }

    /// A logger that keeps the file open and appends, then a rotation by rename and create.
    #[test]
    fn events_for_append_and_rotation() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("app.log");
        std::fs::write(&path, b"one\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let w = Watcher::watch(&path, move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        assert!(!w.is_polling());
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        for _ in 0..3 {
            f.write_all(b"two\n").unwrap();
            assert!(wait(&rx), "no event for an append while the writer keeps the file open");
        }
        drop(f);
        std::thread::sleep(Duration::from_millis(100));
        while rx.try_recv().is_ok() {}

        std::fs::rename(&path, d.path().join("app.log.1")).unwrap();
        assert!(wait(&rx), "no event for the rename");
        std::fs::write(&path, b"fresh\n").unwrap();
        assert!(wait(&rx), "no event for the new file");
        drop(w);
    }

    /// Reading the file (what `refresh` and the table do) must not wake the watcher.
    #[test]
    fn own_reads_are_quiet() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("app.log");
        std::fs::write(&path, b"one\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let _w = Watcher::watch(&path, move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        for _ in 0..5 {
            let _ = std::fs::read(&path).unwrap();
        }
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "woken by a read");
    }

    #[test]
    fn bad_path() {
        assert!(Watcher::watch(Path::new("/"), || {}).is_err());
    }
}
