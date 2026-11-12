//! Search throughput on a `cargo xtask fixtures` file opened through `LogFile` (memory-mapped,
//! indexed first): MB/s per query, best of 3 after a warm-up run, so the page cache is warm. All
//! cores unless the row says `1 thread`.
//!
//! ```text
//! cargo xtask fixtures --size 1G --format jsonl
//! cargo bench -p strata-core --bench search            # STRATA_FIXTURE=path to pick a file
//! ```
//!
//! Without a fixture it prints how to make one and exits successfully.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use strata_core::search::{count_in, search_in};
use strata_core::{FileId, Filter, LogFile, Query, never_cancel};

fn fixture() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("STRATA_FIXTURE") {
        return Some(PathBuf::from(p));
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benches/fixtures");
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    files.sort();
    files.pop()
}

fn best_of(runs: usize, mut f: impl FnMut() -> u64) -> (f64, u64) {
    let mut n = f(); // warm-up
    let mut best = f64::MAX;
    for _ in 0..runs {
        let t = Instant::now();
        n = f();
        best = best.min(t.elapsed().as_secs_f64());
    }
    (best, n)
}

fn main() {
    // `cargo test --all-targets` runs benches as tests without `--bench`: do nothing there.
    if !std::env::args().any(|a| a == "--bench") {
        return;
    }
    let Some(path) = fixture() else {
        eprintln!("no fixture: run `cargo xtask fixtures --size 1G --format jsonl` (or set STRATA_FIXTURE)");
        return;
    };
    let log = LogFile::open(FileId(1), &path).expect("open fixture");
    let t = Instant::now();
    log.index_all(64 << 20, &never_cancel, &mut |_, _| {}).expect("index");
    let mb = log.len() as f64 / 1e6;
    println!("indexed {} lines in {:.0} ms ({})", log.lines(), t.elapsed().as_secs_f64() * 1e3, log.format().as_str());
    println!("{} ({mb:.0} MB, {} threads)", path.display(), std::thread::available_parallelism().map_or(1, |n| n.get()));

    let q = |text: &str, regex: bool, case_sensitive: bool, whole_word: bool| Query { text: text.into(), regex, case_sensitive, whole_word };
    let cases = [
        ("literal, case-sensitive, rare", q("deadlock detected", false, true, false)),
        ("literal, ignore case, rare", q("deadlock detected", false, false, false)),
        ("literal, ignore case, common", q("cache hit", false, false, false)),
        ("literal, digits", q("r-0059e8ba", false, false, false)),
        ("literal, whole word", q("timeout", false, false, true)),
        ("regex, case-sensitive", q(r#""status":5\d\d"#, true, true, false)),
        ("regex, ignore case", q("upstream (timeout|refused)", true, false, false)),
        ("regex, alternation", q("deadlock|payment declined|invalid signature", true, true, false)),
    ];
    for (name, query) in cases {
        let m = query.compile().unwrap();
        let (secs, n) = best_of(3, || {
            let mut kept = 0u64;
            let s = search_in(&log, &m, None, None, true, &never_cancel, &mut |_| {
                kept += 1;
                kept < 500 // MCP's cap: deliver 500, count the rest
            })
            .unwrap();
            s.matched
        });
        println!("{name:<32} {:>8.0} MB/s  {n:>9} lines  {:>7.1} ms", mb / secs, secs * 1e3);
    }
    // One core: a search over one chunk at a time runs on the calling thread.
    let lines = strata_core::search::LineSource::line_count(&log);
    let chunk = strata_core::limits::CHUNK_LINES as u64;
    for (name, query) in [
        ("1 thread: literal, case-sensitive", q("deadlock detected", false, true, false)),
        ("1 thread: literal, ignore case", q("deadlock detected", false, false, false)),
        ("1 thread: regex, ignore case", q("upstream (timeout|refused)", true, false, false)),
    ] {
        let m = query.compile().unwrap();
        let (secs, n) = best_of(3, || {
            let mut total = 0;
            for start in (0..lines).step_by(chunk as usize) {
                total += search_in(&log, &m, None, Some(start..start + chunk), true, &never_cancel, &mut |_| true).unwrap().matched;
            }
            total
        });
        println!("{name:<32} {:>8.0} MB/s  {n:>9} lines  {:>7.1} ms", mb / secs, secs * 1e3);
    }
    {
        let f = Filter::parse("level >= warn and status >= 500").unwrap();
        let all = strata_core::Matcher::all();
        let (secs, n) = best_of(1, || {
            let mut total = 0;
            for start in (0..lines).step_by(chunk as usize) {
                total += search_in(&log, &all, Some(&f), Some(start..start + chunk), true, &never_cancel, &mut |_| true).unwrap().matched;
            }
            total
        });
        println!("{:<32} {:>8.0} MB/s  {n:>9} lines  {:>7.1} ms", "1 thread: field filter", mb / secs, secs * 1e3);
    }
    let filters = ["error", "level >= warn", "level >= warn and status >= 500", "status >= 500 and service = api"];
    for summarized in [false, true] {
        if summarized {
            let t = Instant::now();
            for c in 0..log.with_index(|ix| ix.chunks()) {
                log.summarize_chunk(c, &never_cancel).expect("summarize");
            }
            println!("summarized every chunk in {:.0} ms (1 thread)", t.elapsed().as_secs_f64() * 1e3);
        }
        for text in filters {
            let f = Filter::parse(text).unwrap();
            let (secs, n) = best_of(3, || count_in(&log, &f, &never_cancel).unwrap());
            println!("{:<32} {:>8.0} MB/s  {n:>9} lines  {:>7.1} ms", format!("count `{text}`"), mb / secs, secs * 1e3);
        }
    }
    for (name, f) in [("histogram auto", None), ("histogram `level >= warn`", Some(Filter::parse("level >= warn").unwrap()))] {
        let (secs, n) = best_of(3, || strata_core::histogram::histogram(&log, None, f.as_ref(), &never_cancel).unwrap().buckets.len() as u64);
        println!("{name:<32} {:>8.0} MB/s  {n:>9} bkts   {:>7.1} ms", mb / secs, secs * 1e3);
    }
    let (secs, n) = best_of(1, || strata_core::stats::stats(&log, None, None, 10, 5, &never_cancel).unwrap().fields.len() as u64);
    println!("{:<32} {:>8.0} MB/s  {n:>9} flds   {:>7.1} ms", "stats", mb / secs, secs * 1e3);
}
