#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The window, end to end: every scene against its golden snapshot and accessibility tree, the
//! `.ui` script that opens a fixture, searches and follows, and the idle check (following a file
//! that does not change draws no frames; an appended line does).
//!
//! `ASHLAR_UPDATE_SNAPSHOTS=1` records new PNG baselines (per GPU adapter; another adapter's are
//! skipped with a note); `STRATA_UPDATE_GOLDEN=1` rewrites the tree goldens.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use ashlar::app::{Driver, Renderer};
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use strata::StrataDoc;

/// The files scenes write into `scenes::fixture_dir()`.
const FIXTURE_FILES: [&str; 2] = ["core-dump.bin", strata::scenes::FIXTURE_NAME];

fn driver(dir: &Path, pixels: bool) -> Driver<StrataDoc> {
    let rt = strata::app().dirs(AppDirs::under(dir)).runtime();
    let mut d = if pixels { Driver::with_renderer(rt, Renderer::wgpu()) } else { Driver::new(rt) };
    d.set_screen(1280.0, 800.0);
    d.pixels_per_point(1.0);
    d.settle(60);
    d.call("engine.execute", json!({"command": "view.theme", "params": {"mode": "dark"}})).unwrap();
    d.settle(60);
    d
}

/// The accessibility tree without per-run details (element refs, the temp folder, the home).
fn tree(d: &mut Driver<StrataDoc>) -> String {
    let raw = d.call("ui.snapshot", json!({})).unwrap()["tree"].as_str().unwrap_or_default().to_string();
    let scenes = strata::scenes::fixture_dir().display().to_string();
    let scenes_tilde = strata::views::fmt::tilde(&strata::scenes::fixture_dir());
    let mut out = String::new();
    for line in raw.lines() {
        // shortcuts as the Mac writes them, so every platform compares with the same golden
        let mut l = line
            .replace(&scenes, "<scenes>")
            .replace(&scenes_tilde, "<scenes>")
            .replace("Ctrl+Shift+", "⌘⇧")
            .replace("Ctrl+", "⌘")
            .replace('\\', "/");
        // a path cut to fit ("/var/folders/…/strata-scen…"): its length differs per machine
        if let (Some(a), Some(b)) = (l.find('"'), l.rfind("…\""))
            && b > a
            && [FIXTURE_FILES[0], FIXTURE_FILES[1]]
                .iter()
                .flat_map(|f| [format!("{scenes}/{f}"), format!("{scenes_tilde}/{f}")])
                .any(|p| p.replace('\\', "/").starts_with(&l[a + 1..b]))
        {
            l.replace_range(a + 1..b, "<scenes>");
        }
        while let Some(at) = l.find(" [ref=") {
            let end = l[at + 1..].find(']').map_or(l.len(), |e| at + 2 + e);
            l.replace_range(at..end, "");
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

fn check_tree(name: &str, got: &str) {
    let path = Path::new("tests/snapshots").join(format!("{name}.tree.txt"));
    if std::env::var("STRATA_UPDATE_GOLDEN").is_ok_and(|v| v == "1") || !path.exists() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap();
    if want != got {
        std::fs::write(path.with_extension("new.tree.txt"), got).unwrap();
        // the changed lines, so a failure on a CI machine says what differs
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
        let mut diff = String::new();
        for i in 0..w.len().max(g.len()) {
            let (a, b) = (w.get(i).copied().unwrap_or("<none>"), g.get(i).copied().unwrap_or("<none>"));
            if a != b {
                diff.push_str(&format!("  line {}:\n    want {a}\n    got  {b}\n", i + 1));
            }
        }
        panic!("{name}: the accessibility tree changed (see {name}.new.tree.txt; STRATA_UPDATE_GOLDEN=1 accepts it)\n{diff}");
    }
}

#[test]
fn every_scene_matches_its_golden_snapshot_and_tree() {
    for name in strata::scenes::NAMES {
        let dir = tempfile::tempdir().unwrap();
        let mut d = driver(dir.path(), true);
        d.scene(name).unwrap();
        d.settle(60);
        let t = tree(&mut d);
        assert!(t.contains("strata.status") || t.contains("strata.empty"), "{name}: {t}");
        check_tree(name, &t);
        d.assert_snapshot(&format!("scene-{name}")).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn scenes_show_their_screen() {
    let expect: &[(&str, &[&str])] = &[
        ("empty", &["strata.empty.open", "Drop a log file here"]),
        ("indexing", &["Indexing 38 %", "strata.indexing.stop"]),
        ("main", &["360 lines", "Line 34", "2 matches", "Error 13", "strata.table"]),
        ("agent", &["strata.agent", "Line 34"]),
        ("settings", &["strata.settings", "Theme"]),
        ("no-matches", &["No lines match these filters", "service = billing", "strata.table.clear_filters"]),
        ("file-replaced", &["The file was replaced", "● following"]),
        ("unreadable", &["Can't open this file", "binary file"]),
    ];
    for (name, wants) in expect {
        let dir = tempfile::tempdir().unwrap();
        let mut d = driver(dir.path(), false);
        d.scene(name).unwrap();
        d.settle(60);
        let t = tree(&mut d);
        for w in *wants {
            assert!(t.contains(w), "{name}: no `{w}` in\n{t}");
        }
    }
}

#[test]
fn the_script_opens_searches_and_follows() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path(), false);
    let script = std::fs::read_to_string("tests/scripts/open-search-follow.ui").unwrap();
    d.run_ui(&script).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn filters_levels_and_columns_change_the_table() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path(), false);
    let exec = |d: &mut Driver<StrataDoc>, id: &str, p: Value| {
        let r = d.call("engine.execute", json!({"command": id, "params": p})).unwrap_or_else(|e| panic!("{id}: {e}"));
        d.settle(60);
        r
    };
    exec(&mut d, "log.open_dialog", json!({"path": "tests/fixtures/payments.jsonl"}));
    exec(&mut d, "filter.level", json!({"level": "info", "on": false}));
    assert!(tree(&mut d).contains("1 filter · 89 shown"), "{}", tree(&mut d));
    exec(&mut d, "detail.filter_by_value", json!({"field": "service", "value": "auth"}));
    let t = tree(&mut d);
    assert!(t.contains("service = auth") && t.contains("2 filters"), "{t}");
    exec(&mut d, "view.toggle_column", json!({"column": "status", "on": true}));
    assert!(tree(&mut d).contains("strata.table.header.4"), "a fifth column");
    exec(&mut d, "app.undo", json!({}));
    exec(&mut d, "app.undo", json!({}));
    exec(&mut d, "app.undo", json!({}));
    let t = tree(&mut d);
    assert!(!t.contains("strata.status.filters"), "{t}");
    exec(&mut d, "view.select", json!({"line": 34}));
    exec(&mut d, "bookmark.here", json!({}));
    assert!(tree(&mut d).contains("Line 34"), "the bookmark is in the sidebar");
    let copied = exec(&mut d, "detail.copy_line", json!({}));
    assert!(copied["text"].as_str().unwrap().contains("connection refused"));
}

#[test]
fn following_an_unchanged_file_draws_nothing_and_new_lines_wake_it() {
    let dir = tempfile::tempdir().unwrap();
    // the log in a folder of its own: the app's settings and autosave go elsewhere
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("live.log");
    std::fs::copy("tests/fixtures/payments.jsonl", &log).unwrap();
    let mut d = driver(dir.path(), false);
    d.call("engine.execute", json!({"command": "log.open_dialog", "params": {"path": log.display().to_string(), "follow": true}})).unwrap();
    // let the watcher start and any startup work finish
    for _ in 0..20 {
        d.settle(120);
        std::thread::sleep(Duration::from_millis(20));
    }
    // the settings written once after an open (Recent) are due a little later
    d.advance(5.0);
    d.settle(120);
    let before = d.frames();
    let wakes0 = d.call("app.stats", json!({})).unwrap()["wakes"].clone();
    let ran = d.advance(10.0);
    std::thread::sleep(Duration::from_millis(300));
    let ran = ran + d.advance(10.0);
    let wakes1 = d.call("app.stats", json!({})).unwrap()["wakes"].clone();
    assert_eq!(ran, 0, "an idle window following an unchanged file ran {ran} frames ({before} before): {wakes0} -> {wakes1}");
    // an appended line wakes it and shows up
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    writeln!(f, r#"{{"ts":"2026-11-12T00:00:09.000Z","level":"error","service":"api","msg":"appended"}}"#).unwrap();
    f.sync_all().unwrap();
    let start = Instant::now();
    let mut seen = false;
    while start.elapsed() < Duration::from_secs(10) {
        d.advance(0.1);
        if tree(&mut d).contains("361 lines") {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(seen, "the appended line never showed: {}", tree(&mut d));
}
