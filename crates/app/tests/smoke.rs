#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The app starts, draws the empty state, registers every command, applies every scene and runs
//! headless from the real binary.

use std::process::Command;

use ashlar::app::Driver;
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use strata::StrataDoc;
use strata::commands::ids;

fn driver(dir: &std::path::Path) -> Driver<StrataDoc> {
    let mut d = Driver::new(strata::app().dirs(AppDirs::under(dir)).runtime());
    d.settle(60);
    d
}

fn tree(d: &mut Driver<StrataDoc>) -> String {
    d.call("ui.snapshot", json!({})).unwrap()["tree"].as_str().unwrap_or_default().to_string()
}

#[test]
fn the_window_shows_the_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    let t = tree(&mut d);
    assert!(t.contains("strata.empty.open") && t.contains("Open a file"), "{t}");
}

#[test]
fn every_command_is_registered_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    let listed = d.call("engine.commands", json!({})).unwrap().to_string();
    for id in ids::AGENT.iter().chain(ids::USER) {
        assert!(listed.contains(&format!("\"{id}\"")), "{id} not listed");
    }
    let files = d.call("engine.execute", json!({"command": ids::LOG_FILES, "params": {}})).unwrap();
    assert_eq!(files["result"].get("files").or(files.get("files")), Some(&json!([])), "{files}");
}

#[test]
fn every_scene_applies() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    for name in strata::scenes::NAMES {
        d.scene(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        d.settle(60);
    }
    assert_eq!(d.call("ui.scene", json!({})).unwrap(), json!({"scenes": strata::scenes::NAMES}));
}

#[test]
fn every_undo_label_is_distinct() {
    let mut seen = std::collections::HashSet::new();
    for l in strata::doc::labels::ALL {
        assert!(seen.insert(*l), "{l} twice");
    }
}

#[test]
fn headless_binary_writes_a_snapshot_and_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (snap, tree) = (dir.path().join("out.png"), dir.path().join("tree.json"));
    let out = Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["--headless", "--size", "640x400", "--scale", "1"])
        .arg("--snap")
        .arg(&snap)
        .arg("--tree")
        .arg(&tree)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let png = std::fs::read(&snap).unwrap();
    assert_eq!(png.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
    let tree = std::fs::read_to_string(&tree).unwrap();
    assert!(tree.contains("strata.empty.open"), "{tree}");
}

#[test]
fn the_cli_lists_the_commands() {
    let out = Command::new(env!("CARGO_BIN_EXE_strata-cli")).args(["commands", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("log.search") && text.contains("agent.undo"), "{text}");
}
