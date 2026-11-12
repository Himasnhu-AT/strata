//! End to end: a `cargo xtask fixtures` file opened and indexed through `LogFile`, then searched,
//! filtered on fields, counted into a histogram and summarised, each checked against a naive pass
//! over the same lines.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use strata_core::search::{count, find_next, search};
use strata_core::{FileId, Filter, FormatKind, Level, LogFile, Query, histogram, never_cancel, stats};

// The fixture generator itself (xtask's module), so the test makes the same bytes as
// `cargo xtask fixtures`.
#[allow(dead_code, clippy::all)]
#[path = "../../../xtask/src/fixtures.rs"]
mod fixtures;

#[allow(dead_code)]
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Row {
    text: String,
    level: Option<Level>,
    status: Option<f64>,
}

fn naive(path: &std::path::Path) -> Vec<Row> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            Row { text: l.to_string(), level: v["level"].as_str().and_then(Level::parse), status: v["status"].as_f64() }
        })
        .collect()
}

fn open(path: &std::path::Path) -> LogFile {
    let file = LogFile::open(FileId(1), path).unwrap();
    file.index_all(4 << 20, &never_cancel, &mut |_, _| {}).unwrap();
    assert_eq!(file.format(), FormatKind::JsonLines);
    file
}

#[test]
fn search_filter_histogram_stats_on_a_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.jsonl");
    let args: Vec<String> =
        ["--size", "24M", "--format", "jsonl", "--seed", "7", "--out"].iter().map(|s| s.to_string()).chain([path.display().to_string()]).collect();
    fixtures::main(&args).unwrap();
    let rows = naive(&path);
    let file = open(&path);
    assert_eq!(file.lines(), rows.len() as u64);
    assert!(file.with_index(|ix| ix.chunks()) >= 3, "want several chunks");

    // text search, in order, with spans on the right bytes
    let m = Query::text("DEADLOCK detected").compile().unwrap();
    let mut got = Vec::new();
    let s = search(&file, &m, None, None, true, &never_cancel, &mut |hit| {
        got.push(hit);
        true
    })
    .unwrap();
    let want: Vec<u64> =
        rows.iter().enumerate().filter(|(_, r)| r.text.to_lowercase().contains("deadlock detected")).map(|(i, _)| i as u64).collect();
    assert!(!want.is_empty());
    assert_eq!(got.iter().map(|h| h.line).collect::<Vec<_>>(), want);
    assert!(s.complete && s.scanned == file.lines());
    for hit in got.iter().take(20) {
        let line = file.line(hit.line, usize::MAX).unwrap();
        let span = hit.spans[0];
        assert_eq!(&line.bytes[span.start as usize..span.end as usize], b"deadlock detected");
    }

    // a field filter, before and after the chunks are summarized
    let wanted = |r: &Row| r.level.is_some_and(|l| l >= Level::Warn) && r.status.is_some_and(|s| s >= 500.0);
    let expected = rows.iter().filter(|r| wanted(r)).count() as u64;
    assert!(expected > 0);
    let f = Filter::parse("level >= warn and status >= 500").unwrap();
    assert_eq!(count(&file, &f, &never_cancel).unwrap(), expected);
    let first = rows.iter().position(wanted).unwrap() as u64;
    let last = rows.iter().rposition(wanted).unwrap() as u64;
    for c in 0..file.with_index(|ix| ix.chunks()) {
        file.summarize_chunk(c, &never_cancel).unwrap();
    }
    assert_eq!(count(&file, &f, &never_cancel).unwrap(), expected);
    if first > 0 {
        assert_eq!(find_next(&file, &f, first - 1, false, &never_cancel).unwrap(), Some(first));
    }
    assert_eq!(find_next(&file, &f, u64::MAX, true, &never_cancel).unwrap(), Some(last));
    let errors = Filter::parse("level >= error").unwrap();
    let n_errors = rows.iter().filter(|r| r.level.is_some_and(|l| l >= Level::Error)).count() as u64;
    assert_eq!(count(&file, &errors, &never_cancel).unwrap(), n_errors);

    // search restricted by a filter
    let m = Query::text("upstream").compile().unwrap();
    let s = search(&file, &m, Some(&errors), None, true, &never_cancel, &mut |_| true).unwrap();
    let both = rows.iter().filter(|r| r.text.contains("upstream") && r.level.is_some_and(|l| l >= Level::Error)).count() as u64;
    assert_eq!(s.matched, both);

    // histogram: every line lands somewhere, the filter is applied
    let h = histogram::histogram(&file, None, None, &never_cancel).unwrap();
    assert!(h.buckets.len() <= strata_core::limits::HISTOGRAM_MAX);
    assert_eq!(h.buckets.iter().map(|b| b.counts.total()).sum::<u64>() + h.untimed, file.lines());
    let he = histogram::histogram(&file, Some(60_000_000), Some(&errors), &never_cancel).unwrap();
    assert_eq!(he.width, 60_000_000);
    assert_eq!(he.buckets.iter().map(|b| b.counts.total()).sum::<u64>() + he.untimed, n_errors);

    // stats
    let st = stats::stats(&file, None, None, 10, 5, &never_cancel).unwrap();
    assert_eq!(st.lines, file.lines());
    assert_eq!(st.levels.get(Level::Error) + st.levels.get(Level::Fatal), n_errors);
    assert!(st.first_time.is_some() && st.first_time <= st.last_time);
    let service = st.fields.iter().find(|f| f.name == "service").expect("service field");
    assert_eq!(service.lines, file.lines());
    assert_eq!(service.ty, stats::FieldType::String);
    let status = st.fields.iter().find(|f| f.name == "status").expect("status field");
    assert_eq!(status.ty, stats::FieldType::Number);
}
