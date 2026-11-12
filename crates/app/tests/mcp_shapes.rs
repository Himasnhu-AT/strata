#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The MCP result contract: `tests/mcp/<command>.json` is the golden result of each agent
//! command (`strata::results::*::example()`, serialized). This test checks that
//!
//! - every agent command has a golden file and every golden file is an agent command;
//! - the example types still serialize to the golden shape (keys and JSON types);
//! - whatever a command returns today through a headless session has that shape too (stubs fail
//!   with "not yet"; once a stream fills a command in, its output is checked here).
//!
//! After an additive contract change, regenerate the files with
//! `STRATA_UPDATE_GOLDEN=1 cargo test -p strata --test mcp_shapes` and review the diff.

use std::path::PathBuf;

use ashlar::prelude::*;
use serde_json::Value;
use strata::commands::ids;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("mcp")
}

fn golden(id: &str) -> Value {
    let path = golden_dir().join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Where `actual` differs in shape from `expected`: same object keys, same JSON types (integers
/// and fractions apart), each array element shaped like the expected array's first element.
/// `null` on either side matches anything (an optional value).
fn shape_diff(expected: &Value, actual: &Value, at: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Null, _) | (_, Value::Null) => {}
        (Value::Object(e), Value::Object(a)) => {
            for k in e.keys().filter(|k| !a.contains_key(*k)) {
                out.push(format!("{at}.{k}: missing"));
            }
            for k in a.keys().filter(|k| !e.contains_key(*k)) {
                out.push(format!("{at}.{k}: not in the contract"));
            }
            for (k, ev) in e {
                if let Some(av) = a.get(k) {
                    shape_diff(ev, av, &format!("{at}.{k}"), out);
                }
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if let Some(first) = e.first() {
                for (i, av) in a.iter().enumerate() {
                    shape_diff(first, av, &format!("{at}[{i}]"), out);
                }
            }
        }
        (Value::Number(e), Value::Number(a)) => {
            if e.is_f64() != a.is_f64() {
                out.push(format!("{at}: number kind {e} vs {a}"));
            }
        }
        (Value::String(_), Value::String(_)) | (Value::Bool(_), Value::Bool(_)) => {}
        (e, a) => out.push(format!("{at}: expected {e}, got {a}")),
    }
}

fn assert_shape(id: &str, expected: &Value, actual: &Value) {
    let mut diffs = Vec::new();
    shape_diff(expected, actual, id, &mut diffs);
    assert!(diffs.is_empty(), "{id} result does not match tests/mcp/{id}.json:\n{}", diffs.join("\n"));
}

/// Valid parameters for each agent command.
fn params(id: &str) -> Value {
    match id {
        ids::LOG_OPEN => json!({"path": "/no/such/strata-test.log"}),
        ids::LOG_SEARCH => json!({"query": "timeout"}),
        ids::LOG_LINES => json!({"from": 1, "count": 10}),
        ids::LOG_AROUND => json!({"line": 1}),
        ids::LOG_FIRST | ids::VIEW_FILTER => json!({"filter": "level = error"}),
        ids::VIEW_GOTO => json!({"line": 1}),
        ids::VIEW_HIGHLIGHT => json!({"lines": [1, 2]}),
        ids::BOOKMARK_ADD => json!({"line": 1, "name": "here"}),
        ids::AGENT_NOTE => json!({"text": "found it"}),
        _ => json!({}),
    }
}

#[test]
fn golden_files_match_the_result_types() {
    let update = std::env::var_os("STRATA_UPDATE_GOLDEN").is_some();
    for id in ids::AGENT {
        let example = strata::results::example(id).unwrap_or_else(|| panic!("{id}: no example in results.rs"));
        if update {
            let text = serde_json::to_string_pretty(&example).unwrap() + "\n";
            std::fs::write(golden_dir().join(format!("{id}.json")), text).unwrap();
        }
        let g = golden(id);
        assert_shape(id, &g, &example);
        assert_shape(id, &example, &g);
    }
    let mut files: Vec<String> =
        std::fs::read_dir(golden_dir()).unwrap().filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(str::to_string)).collect();
    files.sort();
    let mut want: Vec<String> = ids::AGENT.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(files, want, "one golden file per agent command, no others");
}

#[test]
fn command_results_have_the_golden_shape() {
    let mut h = strata::app().headless();
    let listed: Vec<String> = h.commands().into_iter().map(|c| c.id).collect();
    for id in ids::AGENT {
        assert!(listed.iter().any(|l| l == id), "{id} is not registered");
        match h.execute(id, params(id)) {
            Ok(result) => assert_shape(id, &golden(id), &result),
            // stubs (and real failures, like the missing file above) are errors, never panics
            Err(e) => assert!(!e.to_string().is_empty(), "{id}"),
        }
    }
}
