//! Parse and redaction throughput per format on `cargo xtask fixtures` output.
//!
//! ```text
//! for f in jsonl logfmt plain syslog access; do cargo xtask fixtures --size 50M --format $f; done
//! cargo bench -p strata-core --bench formats
//! ```
//!
//! Reads `benches/fixtures/<format>-*` (or the files given as arguments), detects each file's
//! format from its first 64 KB, then times parsing every line (one reused `Parsed`) and redacting
//! every line. Prints lines/s and MB/s. Without fixtures it says so and exits.

use std::path::{Path, PathBuf};
use std::time::Instant;

use strata_core::formats::{self, FieldNames};
use strata_core::limits::{DETECT_BYTES, MAX_LINE_BYTES};
use strata_core::{Parsed, Redaction};

fn fixtures() -> Vec<PathBuf> {
    let args: Vec<PathBuf> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).map(PathBuf::from).collect();
    if !args.is_empty() {
        return args;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benches/fixtures");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    files.sort();
    files
}

fn main() {
    let files = fixtures();
    if files.is_empty() {
        println!("no fixtures: run `cargo xtask fixtures --format <f>` for jsonl, logfmt, plain, syslog, access");
        return;
    }
    println!("{:<34} {:>11} {:>8} {:>13} {:>9} {:>13} {:>9}", "file", "format", "conf", "parse lines/s", "MB/s", "redact lines/s", "MB/s");
    for path in files {
        let Ok(data) = std::fs::read(&path) else { continue };
        let det = formats::detect_scored(data.get(..DETECT_BYTES).unwrap_or(&data));
        let parser = formats::parser(det.kind);
        let names = FieldNames::default();
        let lines: Vec<&[u8]> = data.split(|&c| c == b'\n').filter(|l| !l.is_empty()).map(|l| l.get(..MAX_LINE_BYTES).unwrap_or(l)).collect();
        let bytes = data.len() as f64;

        let mut out = Parsed::default();
        let mut with_time = 0u64;
        // Warm up the field table, then time.
        for l in lines.iter().take(10_000) {
            parser.parse_into(l, &names, &mut out);
        }
        let t = Instant::now();
        for l in &lines {
            parser.parse_into(l, &names, &mut out);
            with_time += u64::from(out.time.is_some());
        }
        let parse = t.elapsed().as_secs_f64();

        let red = Redaction::default();
        let mut redacted = 0u64;
        let t = Instant::now();
        for l in &lines {
            let s = String::from_utf8_lossy(l);
            redacted += u64::from(matches!(red.redact(&s), std::borrow::Cow::Owned(_)));
        }
        let redact = t.elapsed().as_secs_f64();

        let n = lines.len() as f64;
        let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        println!(
            "{name:<34} {:>11} {:>8.2} {:>13.0} {:>9.0} {:>13.0} {:>9.0}   ({} of {} lines with a time, {} redacted)",
            det.kind.as_str(),
            det.confidence,
            n / parse,
            bytes / parse / 1e6,
            n / redact,
            bytes / redact / 1e6,
            with_time,
            lines.len(),
            redacted
        );
    }
}
