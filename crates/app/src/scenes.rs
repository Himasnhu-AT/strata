//! Named scenes, one per screen and state of docs/DESIGN.md (`strata --scene NAME`,
//! `Driver::scene`, MCP `ui_scene`). The names are frozen (lead); the setups are the `ui` stream's.
//!
//! Scenes open a small plain-text `.log` fixture (built into the binary, written to a temp folder)
//! and index it inline, so the first frame is complete and the same every time.

use std::path::PathBuf;

use ashlar::prelude::*;

use crate::commands::S;
use crate::commands::user;
use crate::doc::{Actor, FilterChip, labels, view_keys};
use crate::views::req::keys;

/// The fixture every scene opens (629 lines of a small service mesh's plain-text logs, stack
/// traces included: `cargo xtask fixtures --size 64K --seed 6`).
pub const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/payments.log");
/// Its file name.
pub const FIXTURE_NAME: &str = "payments-api.log";

/// Every scene name, in docs/DESIGN.md order.
pub const NAMES: &[&str] =
    &["empty", "indexing", "main", "agent", "palette", "settings", "no-matches", "file-replaced", "unreadable", "agent-disconnected"];

/// The folder scene files are written to: `/tmp/strata-scenes` on Unix (the same on every
/// machine, so snapshots that show a path match), else the temp folder.
pub fn fixture_dir() -> PathBuf {
    let tmp = if cfg!(unix) && std::path::Path::new("/tmp").is_dir() { PathBuf::from("/tmp") } else { std::env::temp_dir() };
    let dir = tmp.join("strata-scenes");
    std::fs::create_dir_all(&dir).ok();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// Write `bytes` as `name` in the scene folder (once) and return its path. Writers take turns:
/// on Windows a file another scene has open cannot be replaced, and a failed write still counts
/// when the file already holds `bytes` (another writer, or a scanner holding it, got there first).
fn fixture(name: &str, bytes: &[u8]) -> Option<PathBuf> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    let holds = || std::fs::read(&path).ok().as_deref() == Some(bytes);
    if !holds() && ashlar::fs::atomic_write(&path, bytes).is_err() && !holds() {
        return None;
    }
    Some(path)
}

/// Open the fixture, index `share` of it (0..=1) and summarize what was indexed.
fn open_fixture(s: &mut S, share: f64) -> Option<strata_core::FileId> {
    let path = fixture(FIXTURE_NAME, FIXTURE)?;
    let id = user::open_path(s, &path, false).ok()?;
    let log = s.doc().live.get(id)?;
    if share >= 1.0 {
        log.index_all(8 << 20, &|| false, &mut |_, _| {}).ok()?;
    } else {
        log.index_more((log.len() as f64 * share) as u64).ok()?;
    }
    let chunks = log.with_index(|ix| ix.chunks());
    for c in 0..chunks {
        log.summarize_chunk(c, &|| false).ok()?;
    }
    Some(id)
}

/// The main window: the fixture with an error line (with a stack trace) selected and a search
/// running.
fn main_window(s: &mut S) -> Option<strata_core::FileId> {
    let id = open_fixture(s, 1.0)?;
    user::select(s, id, 34);
    // show the stack trace under the selected line too
    s.set_view(view_keys::SCROLL_TO, &40u64);
    s.set_view_value(view_keys::SEARCH, json!({"file": id.0, "query": "deadlock", "regex": false, "case": false, "whole_word": false}));
    Some(id)
}

/// The setup of scene `name`.
pub fn setup(name: &'static str) -> impl Fn(&mut S) + Send + Sync + 'static {
    move |s: &mut S| {
        s.set_view(view_keys::SCENE, name);
        match name {
            "indexing" => {
                s.set_view(keys::HOLD_JOBS, &true);
                let _ = open_fixture(s, 0.38);
            }
            "main" | "palette" | "settings" | "agent" | "agent-disconnected" => {
                let _ = main_window(s);
                match name {
                    "agent" => {
                        s.set_view(view_keys::AGENT_PANEL_OPEN, &true);
                        s.set_view(view_keys::AGENT_CLIENT, "Claude");
                    }
                    "agent-disconnected" => s.set_view(view_keys::AGENT_PANEL_OPEN, &true),
                    "palette" => s.request(Effect::OpenPalette),
                    "settings" => s.set_view(view_keys::SETTINGS, "general"),
                    _ => {}
                }
            }
            "no-matches" => {
                if let Some(id) = open_fixture(s, 1.0) {
                    s.edit(labels::ADD_FILTER, |d| {
                        let chip = d.take_id();
                        if let Some(f) = d.file_mut(id) {
                            f.filters.push(FilterChip { id: chip, text: "service = billing".into(), enabled: true, by: Actor::User });
                        }
                    });
                }
            }
            "file-replaced" => {
                if let Some(id) = open_fixture(s, 1.0) {
                    s.edit(labels::FOLLOW, |d| {
                        if let Some(f) = d.file_mut(id) {
                            f.follow = true;
                        }
                    });
                    s.set_view(keys::HOLD_JOBS, &true);
                    s.set_view(keys::FILE_CHANGED, &json!({"file": id.0, "change": "replaced"}));
                }
            }
            "unreadable" => {
                let bytes: Vec<u8> = (0u32..4096).map(|i| if i % 3 == 0 { (i % 251) as u8 } else { 0 }).collect();
                let path = fixture("core-dump.bin", &bytes).unwrap_or_else(|| fixture_dir().join("core-dump.bin"));
                if user::open_path(s, &path, false).is_err() {
                    // what this file says on every OS: on Windows a virus scanner holding the new
                    // file can make the open fail as "access denied" instead, and the scene would
                    // show a different screen from run to run
                    let detail = strata_core::error::Error::Binary { path: path.clone() }.to_string();
                    let reason = crate::views::states::reason(&detail);
                    s.set_view(keys::OPEN_ERROR, &json!({"path": path.display().to_string(), "reason": reason, "detail": detail}));
                }
            }
            _ => {}
        }
    }
}
