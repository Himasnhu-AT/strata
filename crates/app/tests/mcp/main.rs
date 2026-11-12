#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Every agent (MCP) command against a small deterministic fixture (`fixture.jsonl`, 16 JSON
//! lines with a few secrets), with golden values: through a headless session (what `strata-cli
//! exec` and `strata-cli mcp` run), through the real `strata-cli mcp` over stdio, and as a `.ui`
//! script of an agent session in the window (`agent-session.ui`).
//!
//! The commands built on the `search` stream (search, filters, histogram, stats, fields) check
//! their golden values once that stream is merged ([`search_ready`]); until then its stubs return
//! empty results and only the shapes are checked (`tests/mcp_shapes.rs`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ashlar::app::{Driver, Headless};
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use strata::StrataDoc;
use strata::commands::ids;
use strata::doc::{Actor, Bookmark, view_keys};
use strata::mcp::{TimelineRow, keys};
use strata::results::DENIED;
use strata_core::{FileId, RedactKind, Redaction};

/// The secrets in the fixture: none may reach an agent while redaction is on.
const SECRETS: &[&str] = &["ana@example.com", "abcdef0123456789abcdef", "hunter2", "AKIAIOSFODNN7EXAMPLE"];

fn fixture() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("mcp").join("fixture.jsonl");
    std::fs::canonicalize(&p).unwrap_or(p)
}

fn fixture_bytes() -> u64 {
    std::fs::metadata(fixture()).unwrap().len()
}

/// Whether the `search` stream is merged (its stubs reject every filter).
fn search_ready() -> bool {
    strata_core::Filter::parse("level = error").is_ok()
}

fn open() -> Headless<StrataDoc> {
    let mut h = strata::app().headless();
    run(&mut h, ids::LOG_OPEN, json!({"path": fixture()}));
    h
}

fn run(h: &mut Headless<StrataDoc>, id: &str, p: Value) -> Value {
    h.execute(id, p.clone()).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn err(h: &mut Headless<StrataDoc>, id: &str, p: Value) -> String {
    match h.execute(id, p.clone()) {
        Ok(v) => panic!("{id} {p}: expected an error, got {v}"),
        Err(e) => e.to_string(),
    }
}

const L6: &str = r#"{"ts":"2026-11-12T14:01:10.000Z","level":"info","service":"api","msg":"login ok","user":"[redacted:email]"}"#;
const L7: &str = r#"{"ts":"2026-11-12T14:01:15.000Z","level":"debug","service":"api","msg":"auth header Authorization: Bearer [redacted:bearer]"}"#;
const L8: &str = r#"{"ts":"2026-11-12T14:01:20.000Z","level":"info","service":"payments","msg":"charge created","request_id":"r-4820","status":201}"#;
const L10: &str =
    r#"{"ts":"2026-11-12T14:02:07.118Z","level":"error","service":"payments","msg":"upstream timeout","request_id":"r-4821","status":502}"#;
const L11: &str = r#"{"ts":"2026-11-12T14:02:07.200Z","level":"error","service":"payments","msg":"retry failed: upstream timeout","request_id":"r-4821","status":502}"#;
const L12: &str = r#"{"ts":"2026-11-12T14:02:08.000Z","level":"info","service":"api","msg":"db login","password":"[redacted:secret-field]"}"#;
const L14: &str =
    r#"{"ts":"2026-11-12T14:03:00.000Z","level":"error","service":"api","msg":"timeout calling payments","request_id":"r-4822","status":504}"#;

fn f(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value})
}

#[test]
fn log_open_and_log_files() {
    let mut h = strata::app().headless();
    let bytes = fixture_bytes();
    let path = fixture().display().to_string();
    let indexing = json!({"state": "complete", "done_bytes": bytes, "total_bytes": bytes, "error": null});
    let opened = run(&mut h, ids::LOG_OPEN, json!({"path": fixture()}));
    assert_eq!(opened, json!({"file": 1, "path": path, "bytes": bytes, "format": "jsonl", "lines": 16, "following": false, "indexing": indexing}));
    // the same path again shows the same file
    assert_eq!(run(&mut h, ids::LOG_OPEN, json!({"path": fixture()}))["file"], 1);
    let files = run(&mut h, ids::LOG_FILES, json!({}));
    let span = json!({"from": "2026-11-12T14:00:00.000Z", "to": "2026-11-12T14:04:00.000Z"});
    assert_eq!(
        files,
        json!({"files": [{"file": 1, "path": path, "format": "jsonl", "bytes": bytes, "lines": 16, "time_span": span,
            "following": false, "active": true, "indexing": indexing}], "omitted": 0})
    );
    assert!(err(&mut h, ids::LOG_OPEN, json!({"path": "/no/such/strata.log"})).contains("no such file"));
    assert!(err(&mut h, ids::LOG_LINES, json!({"file": 9, "from": 1, "count": 1})).contains("no open file has id 9"));
}

#[test]
fn log_lines_and_log_around() {
    let mut h = open();
    let lines = run(&mut h, ids::LOG_LINES, json!({"from": 6, "count": 3}));
    assert_eq!(
        lines,
        json!({"file": 1, "total_lines": 16, "omitted": 0, "lines": [
            {"line": 6, "time": "2026-11-12T14:01:10.000Z", "level": "info", "message": "login ok", "text": L6, "cut": false,
             "fields": [f("service", "api"), f("user", "[redacted:email]")]},
            {"line": 7, "time": "2026-11-12T14:01:15.000Z", "level": "debug", "message": "auth header Authorization: Bearer [redacted:bearer]",
             "text": L7, "cut": false, "fields": [f("service", "api")]},
            {"line": 8, "time": "2026-11-12T14:01:20.000Z", "level": "info", "message": "charge created", "text": L8, "cut": false,
             "fields": [f("service", "payments"), f("request_id", "r-4820"), f("status", "201")]},
        ]})
    );
    // past the end: what is there, and how many were left out
    let tail = run(&mut h, ids::LOG_LINES, json!({"from": 15, "count": 5}));
    assert_eq!((tail["lines"].as_array().unwrap().len(), &tail["omitted"]), (2, &json!(3)));
    // over the cap: refused by the parameter schema
    assert!(h.execute(ids::LOG_LINES, json!({"from": 1, "count": 501})).is_err());

    let around = run(&mut h, ids::LOG_AROUND, json!({"line": 12, "before": 1, "after": 0}));
    assert_eq!(
        around,
        json!({"file": 1, "line": 12, "lines": [
            {"line": 11, "time": "2026-11-12T14:02:07.200Z", "level": "error", "message": "retry failed: upstream timeout", "text": L11,
             "cut": false, "fields": [f("service", "payments"), f("request_id", "r-4821"), f("status", "502")]},
            {"line": 12, "time": "2026-11-12T14:02:08.000Z", "level": "info", "message": "db login", "text": L12, "cut": false,
             "fields": [f("service", "api"), f("password", "[redacted:secret-field]")]},
        ]})
    );
    // defaults: 10 before and after, clipped to the file
    let a = run(&mut h, ids::LOG_AROUND, json!({"line": 1}));
    assert_eq!(a["lines"].as_array().unwrap().len(), 11);
    assert!(err(&mut h, ids::LOG_AROUND, json!({"line": 17})).contains("line 17 is past the end (16 lines indexed)"));
}

#[test]
fn view_changes_bookmarks_notes_and_agent_undo() {
    let mut h = open();
    // the person's own bookmark: agent.undo leaves it alone
    h.session_mut().edit("Add Bookmark", |d| {
        let id = d.take_id();
        d.file_mut(FileId(1)).unwrap().bookmarks.push(Bookmark { id, line: 0, name: "mine".into(), by: Actor::User, ..Bookmark::default() });
    });
    let hl = run(&mut h, ids::VIEW_HIGHLIGHT, json!({"lines": [11, 10, 10], "note": "the 502s"}));
    assert_eq!(hl, json!({"file": 1, "highlight": 3, "lines": 2, "omitted": 0}));
    assert_eq!(h.session().view::<u64>(view_keys::SCROLL_TO), Some(9));
    let bm = run(&mut h, ids::BOOKMARK_ADD, json!({"line": 10, "name": "first 502", "note": "payments upstream timed out here"}));
    assert_eq!(bm, json!({"file": 1, "bookmark": 4, "line": 10, "name": "first 502"}));
    let named = run(&mut h, ids::BOOKMARK_ADD, json!({"line": 14}));
    assert_eq!(named["name"], "Line 14");
    assert_eq!(run(&mut h, ids::AGENT_NOTE, json!({"text": "payments timed out"})), json!({"note": 6}));
    assert_eq!(run(&mut h, ids::VIEW_GOTO, json!({"line": 10})), json!({"file": 1, "line": 10}));
    assert_eq!(h.session().view::<u64>(view_keys::SELECTED_LINE), Some(9));
    let list = run(&mut h, ids::BOOKMARK_LIST, json!({}));
    assert_eq!(
        list,
        json!({"file": 1, "omitted": 0, "bookmarks": [
            {"bookmark": 2, "line": 1, "name": "mine", "note": null, "color": "amber", "by": "user"},
            {"bookmark": 4, "line": 10, "name": "first 502", "note": "payments upstream timed out here", "color": "amber", "by": "agent"},
            {"bookmark": 5, "line": 14, "name": "Line 14", "note": null, "color": "amber", "by": "agent"},
        ]})
    );
    // bad input is a clear error
    assert!(err(&mut h, ids::VIEW_HIGHLIGHT, json!({"lines": [99]})).contains("line 99 is past the end"));
    assert!(err(&mut h, ids::VIEW_HIGHLIGHT, json!({})).contains("give lines"));
    assert!(err(&mut h, ids::VIEW_GOTO, json!({})).contains("give line or time"));
    assert!(err(&mut h, ids::AGENT_NOTE, json!({"text": "x".repeat(9000)})).contains("the most is 8192"));

    // agent.undo: one step that removes what the agent added, and only that
    assert_eq!(run(&mut h, ids::AGENT_UNDO, json!({})), json!({"undone": 4}));
    let left = run(&mut h, ids::BOOKMARK_LIST, json!({}));
    assert_eq!(left["bookmarks"].as_array().unwrap().len(), 1);
    assert_eq!(left["bookmarks"][0]["name"], "mine");
    let doc = h.session().doc();
    assert!(doc.agent.notes.is_empty() && doc.files[0].highlights.is_empty() && doc.agent.pending.is_empty());
    assert!(err(&mut h, ids::AGENT_UNDO, json!({})).contains("no agent changes"));
    // and Undo brings them back
    assert_eq!(h.session_mut().undo().as_deref(), Some("Undo Agent Changes"));
    assert_eq!(run(&mut h, ids::BOOKMARK_LIST, json!({}))["bookmarks"].as_array().unwrap().len(), 3);
    assert_eq!(h.session().doc().agent.pending.len(), 4);
}

#[test]
fn turning_agent_changes_off_makes_the_agent_read_only() {
    let mut h = open();
    h.session_mut().set_view(view_keys::AGENT_ALLOW_CHANGES, &false);
    for (id, p) in [
        (ids::VIEW_HIGHLIGHT, json!({"lines": [1]})),
        (ids::BOOKMARK_ADD, json!({"line": 1})),
        (ids::VIEW_GOTO, json!({"line": 1})),
        (ids::VIEW_FILTER, json!({"filter": "level = error"})),
        (ids::VIEW_CLEAR_FILTERS, json!({})),
        (ids::AGENT_NOTE, json!({"text": "hi"})),
    ] {
        assert!(err(&mut h, id, p).contains(DENIED), "{id}");
    }
    run(&mut h, ids::LOG_LINES, json!({"from": 1, "count": 2}));
    run(&mut h, ids::BOOKMARK_LIST, json!({}));
    assert!(h.session().doc().files[0].bookmarks.is_empty());
}

#[test]
fn the_timeline_lists_what_the_agent_did() {
    let mut h = open();
    run(&mut h, ids::LOG_LINES, json!({"from": 6, "count": 2}));
    run(&mut h, ids::VIEW_HIGHLIGHT, json!({"lines": [10]}));
    let rows: Vec<TimelineRow> = h.session().view(view_keys::AGENT_TIMELINE).unwrap();
    let got: Vec<(&str, &str, bool)> = rows.iter().map(|r| (r.command.as_str(), r.summary.as_str(), r.changed_view)).collect();
    let name = fixture().file_name().unwrap().to_string_lossy().into_owned();
    let opened = format!("Opened {name} · 16 lines · JSON lines");
    assert_eq!(
        got,
        vec![
            (ids::LOG_OPEN, opened.as_str(), true),
            (ids::LOG_LINES, "Read 2 lines from line 6 · 2 secrets redacted", false),
            (ids::VIEW_HIGHLIGHT, "Highlighted 1 line", true),
        ]
    );
    assert_eq!((rows[2].file, rows[2].line, rows[2].item), (Some(1), Some(10), Some(2)));
    assert_eq!(h.session().view::<bool>(view_keys::AGENT_PANEL_OPEN), Some(true));
}

#[test]
fn search_filters_histogram_stats_and_fields() {
    let mut h = open();
    if !search_ready() {
        eprintln!("the search stream is not merged yet: golden values skipped");
        let s = run(&mut h, ids::LOG_SEARCH, json!({"query": "timeout"}));
        assert_eq!(s["matches"], json!([]));
        return;
    }
    let hit = |line: u64, time: &str, text: &str| json!({"line": line, "time": time, "level": "error", "text": text, "cut": false});
    assert_eq!(
        run(&mut h, ids::LOG_SEARCH, json!({"query": "timeout", "limit": 2})),
        json!({"file": 1, "total": 3, "omitted": 1, "complete": true, "matches": [
            hit(10, "2026-11-12T14:02:07.118Z", L10), hit(11, "2026-11-12T14:02:07.200Z", L11)]})
    );
    assert_eq!(
        run(&mut h, ids::LOG_SEARCH, json!({"query": "TIMEOUT", "filter": "service = api"})),
        json!({"file": 1, "total": 1, "omitted": 0, "complete": true, "matches": [hit(14, "2026-11-12T14:03:00.000Z", L14)]})
    );
    assert_eq!(run(&mut h, ids::LOG_SEARCH, json!({"query": "TIMEOUT", "case": true}))["total"], 0);
    assert!(err(&mut h, ids::LOG_SEARCH, json!({"query": "(", "regex": true})).contains("bad search"));
    assert!(err(&mut h, ids::LOG_SEARCH, json!({"query": "x", "filter": "level >="})).contains("bad filter"));

    let first = run(&mut h, ids::LOG_FIRST, json!({"filter": "level >= error"}));
    assert_eq!((&first["filter"], &first["line"]["line"], &first["complete"]), (&json!("level >= error"), &json!(10), &json!(true)));
    let none = run(&mut h, ids::LOG_FIRST, json!({"filter": "service = nope"}));
    assert_eq!((&none["line"], &none["complete"]), (&Value::Null, &json!(true)));

    assert_eq!(
        run(&mut h, ids::VIEW_FILTER, json!({"filter": "service = payments"})),
        json!({"file": 1, "filter_id": 2, "filter": "service = payments", "visible": 7, "complete": true})
    );
    assert_eq!(run(&mut h, ids::VIEW_FILTER, json!({"filter": "level >= warn"}))["visible"], 5);
    assert_eq!(run(&mut h, ids::VIEW_CLEAR_FILTERS, json!({})), json!({"file": 1, "removed": 2}));
    assert_eq!(run(&mut h, ids::VIEW_GOTO, json!({"time": "2026-11-12T14:02:00Z"})), json!({"file": 1, "line": 9}));
    assert_eq!(run(&mut h, ids::VIEW_HIGHLIGHT, json!({"query": "upstream"})), json!({"file": 1, "highlight": 4, "lines": 3, "omitted": 0}));

    let lv = |debug: u64, info: u64, warn: u64, error: u64, fatal: u64| json!({"trace": 0, "debug": debug, "info": info, "warn": warn, "error": error, "fatal": fatal, "none": 0});
    assert_eq!(
        run(&mut h, ids::LOG_HISTOGRAM, json!({"bucket": "1m"})),
        json!({"file": 1, "bucket": "1m", "untimed": 0, "omitted": 0, "buckets": [
            {"start": "2026-11-12T14:00:00.000Z", "levels": lv(1, 3, 0, 0, 0)},
            {"start": "2026-11-12T14:01:00.000Z", "levels": lv(1, 2, 1, 0, 0)},
            {"start": "2026-11-12T14:02:00.000Z", "levels": lv(0, 2, 1, 2, 0)},
            {"start": "2026-11-12T14:03:00.000Z", "levels": lv(0, 1, 0, 1, 0)},
            {"start": "2026-11-12T14:04:00.000Z", "levels": lv(0, 0, 0, 0, 1)},
        ]})
    );
    assert_eq!(run(&mut h, ids::LOG_HISTOGRAM, json!({"bucket": "1m", "filter": "level >= error"}))["buckets"][2]["levels"], lv(0, 0, 0, 2, 0));
    assert!(err(&mut h, ids::LOG_HISTOGRAM, json!({"bucket": "soon"})).contains("not a time or duration"));

    let stats = run(&mut h, ids::LOG_STATS, json!({}));
    assert_eq!((&stats["lines"], &stats["levels"], &stats["exact"]), (&json!(16), &lv(2, 8, 2, 3, 1), &json!(true)));
    assert_eq!(stats["time_span"], json!({"from": "2026-11-12T14:00:00.000Z", "to": "2026-11-12T14:04:00.000Z"}));
    assert_eq!(
        stats["top_fields"][0],
        json!({"name": "service", "lines": 16, "values": [
        {"value": "api", "count": 8}, {"value": "payments", "count": 7}, {"value": "worker", "count": 1}]})
    );
    let ranged = run(&mut h, ids::LOG_STATS, json!({"range": {"from": "2026-11-12T14:02:00Z", "to": "2026-11-12T14:02:59Z"}}));
    assert_eq!(ranged["lines"], 5);

    let fields = run(&mut h, ids::LOG_FIELDS, json!({}));
    assert_eq!(
        fields["fields"][0],
        json!({"name": "service", "type": "string", "lines": 16, "distinct": 3, "values": [
        {"value": "api", "count": 8}, {"value": "payments", "count": 7}, {"value": "worker", "count": 1}]})
    );
    let status = fields["fields"].as_array().unwrap().iter().find(|f| f["name"] == "status").unwrap().clone();
    assert_eq!((&status["type"], &status["distinct"]), (&json!("number"), &json!(4)));
}

/// Params that make every read-only command return line text (or values) from the fixture.
fn reads() -> Vec<(&'static str, Value)> {
    vec![
        (ids::LOG_LINES, json!({"from": 1, "count": 16})),
        (ids::LOG_AROUND, json!({"line": 8, "before": 10, "after": 10})),
        (ids::LOG_SEARCH, json!({"query": "login"})),
        (ids::LOG_SEARCH, json!({"query": "Bearer"})),
        (ids::LOG_SEARCH, json!({"query": "example.com"})),
        (ids::LOG_SEARCH, json!({"query": "hunter2"})),
        (ids::LOG_SEARCH, json!({"query": "AKIA"})),
        (ids::LOG_FIRST, json!({"filter": "\"db login\""})),
        (ids::LOG_FIRST, json!({"filter": "user ~ example"})),
        (ids::LOG_FIELDS, json!({})),
        (ids::LOG_STATS, json!({})),
        (ids::LOG_HISTOGRAM, json!({})),
        (ids::LOG_FILES, json!({})),
        (ids::BOOKMARK_LIST, json!({})),
    ]
}

#[test]
fn redaction_applies_to_every_command_that_returns_log_text() {
    let mut h = open();
    // every read-only command is covered
    for id in ids::READ_ONLY {
        assert!(reads().iter().any(|(r, _)| r == id), "{id} is not checked for redaction");
    }
    let mut seen = String::new();
    for (id, p) in reads() {
        let out = match h.execute(id, p.clone()) {
            Ok(v) => v.to_string(),
            // filters are the search stream's: its stubs refuse them until it is merged
            Err(_) if !search_ready() => continue,
            Err(e) => panic!("{id} {p}: {e}"),
        };
        for secret in SECRETS {
            assert!(!out.contains(secret), "{id} {p} sent {secret}: {out}");
        }
        seen.push_str(&out);
    }
    for kind in ["email", "bearer", "secret-field"] {
        assert!(seen.contains(&format!("[redacted:{kind}]")), "{kind}");
    }
    if search_ready() {
        assert!(seen.contains("[redacted:aws-key]"));
    }
    // per-kind settings (Settings ▸ AI): email off sends emails, the rest stays redacted
    let mut r = Redaction::default();
    r.set(RedactKind::Email, false);
    h.session_mut().set_view(keys::REDACTION, &r);
    let out = run(&mut h, ids::LOG_LINES, json!({"from": 1, "count": 16})).to_string();
    assert!(out.contains("ana@example.com") && !out.contains("hunter2"), "{out}");
    // redaction off for the file (log.redaction): everything as written
    h.session_mut().edit("Redaction", |d| d.file_mut(FileId(1)).unwrap().redact = false);
    let out = run(&mut h, ids::LOG_LINES, json!({"from": 1, "count": 16})).to_string();
    assert!(SECRETS.iter().all(|s| out.contains(s)), "{out}");
}

#[test]
fn a_gz_log_opens_through_a_temporary_copy() {
    let dir = tempfile::tempdir().unwrap();
    let gz = dir.path().join("app.jsonl.gz");
    std::fs::write(&gz, flate2_like::encode(&std::fs::read(fixture()).unwrap())).unwrap();
    let mut h = strata::app().headless();
    let opened = run(&mut h, ids::LOG_OPEN, json!({"path": gz}));
    assert_eq!((&opened["lines"], &opened["format"]), (&json!(16), &json!("jsonl")));
    assert!(opened["path"].as_str().unwrap().ends_with("app.jsonl.gz"));
}

/// A gzip stream with stored (uncompressed) deflate blocks: enough for a test, no dependency.
mod flate2_like {
    pub fn encode(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        let chunks: Vec<&[u8]> = data.chunks(65_535).collect();
        for (i, c) in chunks.iter().enumerate() {
            out.push(u8::from(i + 1 == chunks.len()));
            let n = c.len() as u16;
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(!n).to_le_bytes());
            out.extend_from_slice(c);
        }
        out.extend_from_slice(&crc32(data).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
}

/// `strata-cli mcp` over stdio: initialize, list the tools, open the fixture and read lines.
#[test]
fn strata_cli_mcp_serves_the_agent_commands() {
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_strata-cli")).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let call = |id: u64, command: &str, params: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "execute", "arguments": {"command": command, "params": params}}});
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        call(3, ids::LOG_OPEN, json!({"path": fixture()})),
        call(4, ids::LOG_LINES, json!({"from": 6, "count": 2})),
        call(5, ids::VIEW_HIGHLIGHT, json!({"lines": [10]})),
        call(6, ids::AGENT_UNDO, json!({})),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in &requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let replies: Vec<Value> =
        BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect();
    let _ = child.wait();
    let reply = |id: u64| replies.iter().find(|r| r["id"] == id).cloned().unwrap_or_else(|| panic!("no reply {id}: {replies:?}"));
    let text = |id: u64| -> Value {
        let r = reply(id);
        assert_ne!(r["result"]["isError"], true, "{r}");
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    assert!(reply(1)["result"]["instructions"].as_str().unwrap_or_default().contains("Strata"), "{}", reply(1));
    let tools = reply(2).to_string();
    assert!(tools.contains("\"execute\"") && tools.contains("\"list_commands\""), "{tools}");
    assert_eq!(text(3)["lines"], 16);
    let lines = text(4);
    assert_eq!(lines["lines"][0]["text"], L6);
    assert_eq!(lines["lines"][1]["text"], L7);
    assert!(!lines.to_string().contains("ana@example.com"));
    assert_eq!(text(5)["lines"], 1);
    assert_eq!(text(6), json!({"undone": 1}));
}

/// The `.ui` script of an agent session in the window: the agent works, the panel shows it, the
/// person undoes it with the panel's button, and Undo brings it back.
#[test]
fn an_agent_session_in_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = Driver::new(strata::app().dirs(AppDirs::under(dir.path())).runtime());
    d.settle(60);
    let script = include_str!("agent-session.ui").replace("{OPEN}", &json!({"path": fixture()}).to_string());
    if let Err(e) = d.run_ui(&script) {
        panic!("{e:?}");
    }
    d.settle(60);
    let doc = |d: &mut Driver<StrataDoc>| d.call("engine.execute", json!({"command": ids::BOOKMARK_LIST, "params": {}})).unwrap();
    assert_eq!(doc(&mut d)["bookmarks"], json!([]));
    d.call("engine.execute", json!({"command": "app.undo", "params": {}})).unwrap();
    d.settle(60);
    assert_eq!(doc(&mut d)["bookmarks"].as_array().map(Vec::len), Some(1));
}
