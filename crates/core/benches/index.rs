//! Index timing bench (plain, no harness): open + first chunk, full index throughput, index
//! memory and process RSS.
//!
//! ```text
//! cargo xtask fixtures --size 1G --format jsonl
//! cargo bench -p strata-core --bench index                       # every file in benches/fixtures
//! STRATA_BENCH_FILE=/path/a.log,/path/b.log cargo bench -p strata-core --bench index
//! ```
//!
//! Timings are the median of `STRATA_BENCH_RUNS` runs (default 3), warm cache (one untimed pass
//! first). RSS is measured in a fresh child process per file that only opens and indexes the file
//! once: "rss" is the whole process after the full index (read with `ps`), with the scanned pages
//! of the map released as the index job does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use strata_core::limits::CHUNK_LINES;
use strata_core::{FileId, IndexState, LogFile};

fn files() -> Vec<PathBuf> {
    if let Ok(list) = std::env::var("STRATA_BENCH_FILE") {
        return list.split(',').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benches/fixtures");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    v.retain(|p| p.is_file());
    v.sort();
    v
}

fn rss_kb() -> Option<u64> {
    let out = Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v.get(v.len() / 2).copied().unwrap_or_default()
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1 << 20) as f64
}

/// Child: open and index `path` once, print the RSS in KB.
fn rss_child(path: &Path) {
    let Ok(lf) = LogFile::open(FileId(1), path) else { return };
    let _ = lf.index_all(16 << 20, &|| false, &mut |_, _| {});
    println!("{}", rss_kb().unwrap_or(0));
    std::hint::black_box(lf);
}

fn main() {
    if let Ok(one) = std::env::var("STRATA_BENCH_RSS") {
        rss_child(Path::new(&one));
        return;
    }
    let files = files();
    if files.is_empty() {
        println!("no fixtures: run `cargo xtask fixtures --size 1G --format jsonl` or set STRATA_BENCH_FILE");
        return;
    }
    let runs: usize = std::env::var("STRATA_BENCH_RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(3).max(1);
    for path in files {
        measure(&path, runs);
    }
}

fn measure(path: &Path, runs: usize) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Warm the page cache.
    if let Ok(lf) = LogFile::open(FileId(0), path) {
        let _ = lf.index_all(64 << 20, &|| false, &mut |_, _| {});
    }
    let (mut first, mut full) = (Vec::new(), Vec::new());
    let (mut lines, mut mem) = (0, 0);
    for _ in 0..runs {
        let t = Instant::now();
        let lf = match LogFile::open(FileId(1), path) {
            Ok(lf) => lf,
            Err(e) => {
                println!("{name}: {e}");
                return;
            }
        };
        // The first screen: one chunk of lines, indexed in 1 MB steps.
        while lf.lines() < CHUNK_LINES as u64 {
            match lf.index_more(1 << 20) {
                Ok(IndexState::Running { .. }) => {}
                _ => break,
            }
        }
        first.push(t.elapsed());
        let _ = lf.index_all(16 << 20, &|| false, &mut |_, _| {});
        full.push(t.elapsed());
        lines = lf.lines();
        mem = lf.with_index(|ix| ix.memory_bytes());
    }
    let rss = std::env::current_exe()
        .ok()
        .and_then(|exe| Command::new(exe).env("STRATA_BENCH_RSS", path).output().ok())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok())
        .map_or("?".to_string(), |kb| format!("{:.1} MB", kb as f64 / 1024.0));
    let (first, full) = (median(first), median(full));
    println!(
        "{name}: {:.0} MB, {lines} lines | open + first chunk {:.1} ms | full {:.0} ms = {:.0} MB/s | index {:.1} MB ({:.2} B/line) | rss {rss}",
        mib(size),
        first.as_secs_f64() * 1e3,
        full.as_secs_f64() * 1e3,
        mib(size) / full.as_secs_f64().max(1e-9),
        mib(mem),
        mem as f64 / lines.max(1) as f64,
    );
}
