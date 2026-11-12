//! `cargo xtask bench`: Strata's published benchmarks (docs/PLAN.md section 6, method in
//! docs/benchmarks.md). Manual only: it opens real windows, so CI never runs it.
//!
//! - `cargo xtask bench --quick`: 100 MB files, 3 runs, 10 s idle; a few minutes.
//! - `cargo xtask bench`: 100 MB, 1 GB and 5 GB files, 9 runs, 60 s idle, release-small too.
//! - `--runs N`, `--sizes 100M,1G`, `--formats jsonl,plain`, `--idle SECS`, `--only strata|others|apps`,
//!   `--apps klogg,lnav` (any of strata, console, klogg, sublime, vscode, lnav),
//!   `--no-small` (skip the release-small build), `--out PATH` (default `target/bench/bench-<mode>.json`).
//! - `cargo xtask bench --report A.json [B.json …]`: merge results taken in parts (`--only strata`
//!   and `--only others`, a later key wins) and print the markdown tables again.
//!
//! Strata is measured with its own instruments, never a stopwatch on the screen: the ready file
//! (`STRATA_READY_FILE`, written after the first presented frame), `app.stats` over the control
//! port (frame CPU percentiles, job durations, footprint, process CPU time, wake reasons) and
//! `ui.wait` (answered on the frame where an element first appears). Other apps are measured from
//! the outside (`/usr/bin/time -l`, `ps` sampling, a pseudo-terminal for `less`), and every
//! number says how it was taken. Run the whole thing through memgate:
//! `memgate -- cargo xtask bench --quick`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::bench_others as others;
use crate::bench_strata as strata;

/// One fixture: format name (`jsonl` / `plain`), size label and bytes asked for.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub format: String,
    pub label: String,
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct Options {
    pub quick: bool,
    pub runs: usize,
    pub sizes: Vec<(String, u64)>,
    pub formats: Vec<String>,
    /// `--files jsonl:1G,syslog:1G`: exactly these (format, size); else every size × format.
    pub files: Option<Vec<(String, String, u64)>>,
    pub idle_secs: u64,
    pub strata: bool,
    pub others: bool,
    /// Apps with a window, and lnav (`--only apps`).
    pub apps_on: bool,
    /// Only these apps (`--apps klogg,lnav`); `None`: all.
    pub apps: Option<Vec<String>>,
    pub small: bool,
    pub out: Option<PathBuf>,
}

fn size_of(label: &str) -> Result<u64, String> {
    let (num, mult) = match label.trim().char_indices().last() {
        Some((i, 'M' | 'm')) => (label.get(..i), 1u64 << 20),
        Some((i, 'G' | 'g')) => (label.get(..i), 1 << 30),
        _ => (None, 0),
    };
    num.and_then(|n| n.parse::<u64>().ok())
        .and_then(|n| n.checked_mul(mult))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("bench: bad size `{label}` (100M, 1G, 5G)"))
}

fn options(args: &[String]) -> Result<Options, String> {
    let quick = args.iter().any(|a| a == "--quick");
    let mut o = Options {
        quick,
        runs: if quick { 3 } else { 9 },
        sizes: if quick {
            vec![("100M".into(), 100 << 20)]
        } else {
            vec![("100M".into(), 100 << 20), ("1G".into(), 1 << 30), ("5G".into(), 5 << 30)]
        },
        formats: vec!["plain".into(), "jsonl".into()],
        // the full run: plain text and JSON lines at every size, the other formats at 1 GB
        files: (!quick).then(|| {
            let mut f: Vec<(String, String, u64)> = Vec::new();
            for fmt in ["plain", "jsonl"] {
                for (l, b) in [("100M", 100u64 << 20), ("1G", 1 << 30), ("5G", 5 << 30)] {
                    f.push((fmt.into(), l.into(), b));
                }
            }
            for fmt in ["logfmt", "syslog", "access", "macos"] {
                f.push((fmt.into(), "1G".into(), 1 << 30));
            }
            f
        }),
        idle_secs: if quick { 10 } else { 60 },
        strata: true,
        others: true,
        apps_on: true,
        apps: None,
        small: !quick,
        out: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("bench: {a} needs a value"));
        match a.as_str() {
            "--quick" => {}
            "--runs" => o.runs = value()?.parse::<usize>().map_err(|e| format!("bench: --runs: {e}"))?.clamp(1, 100),
            "--files" => {
                let mut files = Vec::new();
                for item in value()?.split(',') {
                    let (f, size) = item.trim().split_once(':').ok_or_else(|| format!("bench: --files {item}: format:size, e.g. syslog:1G"))?;
                    if !FORMATS.contains(&f) {
                        return Err(format!("bench: unknown format `{f}` ({})", FORMATS.join(", ")));
                    }
                    files.push((f.to_string(), size.to_uppercase(), size_of(size)?));
                }
                o.files = Some(files);
            }
            "--sizes" => {
                o.files = None;
                o.sizes = value()?.split(',').map(|s| size_of(s).map(|b| (s.trim().to_uppercase(), b))).collect::<Result<_, _>>()?;
            }
            "--formats" => {
                o.files = None;
                o.formats = value()?.split(',').map(|s| s.trim().to_string()).collect();
                if let Some(bad) = o.formats.iter().find(|f| !FORMATS.contains(&f.as_str())) {
                    return Err(format!("bench: unknown format `{bad}` (jsonl, plain)"));
                }
            }
            "--idle" => o.idle_secs = value()?.parse::<u64>().map_err(|e| format!("bench: --idle: {e}"))?.clamp(1, 600),
            "--only" => {
                let only = value()?;
                o.strata = only == "strata";
                o.others = only == "others";
                o.apps_on = only == "apps";
                if !(o.strata || o.others || o.apps_on) {
                    return Err(format!("bench: --only {only}: strata, others or apps"));
                }
            }
            "--apps" => {
                o.apps_on = true;
                o.apps = Some(value()?.split(',').map(|s| s.trim().to_string()).collect());
            }
            "--no-small" => o.small = false,
            "--out" => o.out = Some(PathBuf::from(value()?)),
            other => return Err(format!("bench: unknown argument `{other}`")),
        }
    }
    Ok(o)
}

/// `CARGO_TARGET_DIR`, else `<root>/target`.
pub fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"))
}

/// Run a command and return its stdout, trimmed (`None` when it cannot run or fails).
pub fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The machine, as the results state it.
fn machine() -> Value {
    let sysctl = |k: &str| output("sysctl", &["-n", k]);
    let mem_gb = sysctl("hw.memsize").and_then(|m| m.parse::<f64>().ok()).map(|b| b / f64::from(1u32 << 30));
    json!({
        "model": sysctl("hw.model"),
        "cpu": sysctl("machdep.cpu.brand_string"),
        "cores": sysctl("hw.ncpu").and_then(|n| n.parse::<u64>().ok()),
        "memory_gb": mem_gb,
        "os": format!("{} {} ({})", output("sw_vers", &["-productName"]).unwrap_or_default(), output("sw_vers", &["-productVersion"]).unwrap_or_default(), output("sw_vers", &["-buildVersion"]).unwrap_or_default()),
        "arch": std::env::consts::ARCH,
    })
}

/// What else the machine was doing: load averages and memgate's queue.
pub fn load() -> Value {
    json!({
        "loadavg": output("sysctl", &["-n", "vm.loadavg"]),
        "memgate": output("memgate", &["status"]),
    })
}

/// The formats the harness knows: the generator's, and `macos` (a real log, see [`macos_log`]).
pub const FORMATS: &[&str] = &["jsonl", "plain", "logfmt", "syslog", "access", "macos"];

/// The real macOS log the `macos` fixture repeats.
pub const MACOS_SOURCE: &str = "/var/log/install.log";

/// `/var/log/install.log` repeated (whole copies, then whole lines) up to `bytes`: a real log
/// with real line lengths and messages. Not reproducible across Macs (each has its own log): the
/// results record its source size.
fn macos_log(path: &Path, bytes: u64) -> Result<(), String> {
    let src = std::fs::read(MACOS_SOURCE).map_err(|e| format!("bench: {MACOS_SOURCE}: {e}"))?;
    if src.is_empty() {
        return Err(format!("bench: {MACOS_SOURCE} is empty"));
    }
    let mut src = src;
    if !src.ends_with(b"\n") {
        src.push(b'\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| format!("bench: {}: {e}", path.display()))?);
    let mut written = 0u64;
    while written < bytes {
        let left = usize::try_from(bytes - written).unwrap_or(usize::MAX);
        let piece = if left >= src.len() {
            src.as_slice()
        } else {
            // up to the end of the line that crosses the size
            let end = src.iter().skip(left).position(|b| *b == b'\n').map_or(src.len(), |i| left + i + 1);
            src.get(..end).unwrap_or_default()
        };
        std::io::Write::write_all(&mut w, piece).map_err(|e| e.to_string())?;
        written += piece.len() as u64;
    }
    std::io::Write::flush(&mut w).map_err(|e| e.to_string())
}

/// The format Strata detects for a file (from its first 64 KB).
fn detected(path: &Path) -> Option<String> {
    let mut head = vec![0u8; 64 * 1024];
    let n = std::io::Read::read(&mut std::fs::File::open(path).ok()?, &mut head).ok()?;
    Some(strata_core::formats::detect(head.get(..n)?).as_str().to_string())
}

/// Generate the fixture when it is missing (the same bytes for the same size and seed).
fn fixture(root: &Path, format: &str, label: &str, bytes: u64) -> Result<Fixture, String> {
    if format == "macos" {
        let path = root.join("benches").join("fixtures").join(format!("macos-install-{bytes}.log"));
        if !std::fs::metadata(&path).is_ok_and(|m| m.len() >= bytes) {
            eprintln!("bench: writing {}", path.display());
            macos_log(&path, bytes)?;
        }
        return Ok(Fixture { format: format.into(), label: label.into(), path, bytes });
    }
    let ext = if format == "jsonl" { "jsonl" } else { "log" };
    let path = root.join("benches").join("fixtures").join(format!("{format}-{bytes}-s42.{ext}"));
    let complete = std::fs::metadata(&path).is_ok_and(|m| m.len() >= bytes);
    if !complete {
        eprintln!("bench: generating {}", path.display());
        let args: Vec<String> = ["--size", &bytes.to_string(), "--format", format, "--seed", "42", "--out", &path.display().to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        crate::fixtures::main(&args)?;
    }
    Ok(Fixture { format: format.into(), label: label.into(), path, bytes })
}

/// The three searches of one format, written for each tool. `literal` is case-insensitive
/// everywhere; `regex` is an extended regex every tool reads the same; `field` is Strata's filter,
/// `grep_field` its text equivalent (ERE) and `lnav_field` lnav's SQL condition.
pub struct Queries {
    pub literal: &'static str,
    pub regex: &'static str,
    pub field: &'static str,
    pub grep_field: &'static str,
    pub lnav_field: &'static str,
}

pub fn queries(format: &str) -> Queries {
    let ids = "r-[0-9a-f]{6}ff";
    match format {
        "jsonl" => Queries {
            literal: "timeout",
            regex: ids,
            field: "status >= 500",
            grep_field: r#""status":5[0-9][0-9]"#,
            lnav_field: r#"regexp('"status":5[0-9][0-9]', log_raw_text)"#,
        },
        "logfmt" => Queries {
            literal: "timeout",
            regex: ids,
            field: "status >= 500",
            grep_field: " status=5[0-9][0-9] ",
            lnav_field: "regexp(' status=5[0-9][0-9] ', log_raw_text)",
        },
        "syslog" => Queries { literal: "timeout", regex: ids, field: "level = error", grep_field: "^<11>", lnav_field: "log_level = 'error'" },
        "access" => Queries {
            literal: "/v1/pay",
            regex: r#"" 50[0-9] "#,
            field: "status >= 500",
            grep_field: r#"" 5[0-9][0-9] "#,
            lnav_field: r#"regexp('" 5[0-9][0-9] ', log_raw_text)"#,
        },
        "macos" => Queries {
            literal: "error",
            regex: "fail(ed|ure)",
            field: "message ~ error",
            grep_field: "[Ee][Rr][Rr][Oo][Rr]",
            lnav_field: "log_raw_text LIKE '%error%'",
        },
        _ => Queries { literal: "timeout", regex: ids, field: "level = error", grep_field: r"\] ERROR ", lnav_field: "log_level = 'error'" },
    }
}

/// Median, min and max of samples (the median of an even count is the mean of the middle two).
pub fn summary(samples: &[f64]) -> Value {
    let mut v: Vec<f64> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let mid = |i: usize| v.get(i).copied().unwrap_or(0.0);
    let median = if n % 2 == 1 { mid(n / 2) } else { (mid(n / 2 - 1) + mid(n / 2)) / 2.0 };
    let r = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({"median": r(median), "min": r(mid(0)), "max": r(mid(n - 1)), "n": n, "samples": samples.iter().map(|x| r(*x)).collect::<Vec<_>>()})
}

/// Write `results` as JSON to `out` and its tables as markdown next to it.
fn save(results: &Value, out: &Path) -> Result<String, String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("bench: {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(results).map_err(|e| e.to_string())?;
    std::fs::write(out, text + "\n").map_err(|e| format!("bench: {}: {e}", out.display()))?;
    let md = crate::bench_report::markdown(results);
    let md_path = out.with_extension("md");
    std::fs::write(&md_path, &md).map_err(|e| format!("bench: {}: {e}", md_path.display()))?;
    Ok(md)
}

/// What identifies an entry of a results array: its file, its app, or its format and size.
fn entry_id(v: &Value) -> Option<String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    s("file").or(s("app")).map(str::to_string).or_else(|| Some(format!("{} {}", s("format")?, s("size")?)))
}

/// Merge `b` into `a`: objects key by key, arrays of identified entries entry by entry (a later
/// run of the same file replaces it), anything else from `b`.
fn merge(a: &mut Value, b: Value) {
    // an object of plain values (a summary, wake counts) is one measurement: replaced whole
    let leaf = |v: &Value| {
        v.as_object().is_some_and(|o| o.values().all(|x| !x.is_object() && x.as_array().is_none_or(|a| a.iter().all(|y| !y.is_object()))))
    };
    if leaf(&b) {
        *a = b;
        return;
    }
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(old) if !v.is_null() => merge(old, v),
                    Some(_) => {}
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if b.iter().all(|v| entry_id(v).is_some()) && a.iter().all(|v| entry_id(v).is_some()) => {
            for v in b {
                match a.iter_mut().find(|old| entry_id(old) == entry_id(&v)) {
                    Some(old) => merge(old, v),
                    None => a.push(v),
                }
            }
        }
        (a, b) => *a = b,
    }
}

/// `--report A.json B.json [--out C.json]`: merge results taken in parts.
fn report(args: &[String]) -> Result<(), String> {
    let mut merged = Value::Null;
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--out" {
            out = it.next().map(PathBuf::from);
            continue;
        }
        let text = std::fs::read_to_string(a).map_err(|e| format!("bench: {a}: {e}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("bench: {a}: {e}"))?;
        merge(&mut merged, v);
    }
    match out {
        Some(p) => println!("{}", save(&merged, &p)?),
        None => println!("{}", crate::bench_report::markdown(&merged)),
    }
    Ok(())
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    if args.first().is_some_and(|a| a == "--report") {
        return report(args.get(1..).unwrap_or_default());
    }
    let o = options(args)?;
    let mode = if o.quick { "quick" } else { "full" };
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut fixtures = Vec::new();
    match &o.files {
        Some(files) => {
            for (f, label, bytes) in files {
                fixtures.push(fixture(root, f, label, *bytes)?);
            }
        }
        None => {
            for (label, bytes) in &o.sizes {
                for f in &o.formats {
                    fixtures.push(fixture(root, f, label, *bytes)?);
                }
            }
        }
    }
    let load_before = load();
    eprintln!("bench: load before: {load_before}");
    let mut results = json!({
        "schema": 1,
        "mode": mode,
        "runs": o.runs,
        "started_unix": started,
        "date": output("date", &["-u", "+%Y-%m-%d"]),
        "git": output("git", &["-C", &root.display().to_string(), "rev-parse", "--short", "HEAD"]),
        "machine": machine(),
        "load_before": load_before,
        "fixtures": fixtures.iter().map(|f| json!({"format": f.format, "size": f.label, "detected": detected(&f.path), "source": (f.format == "macos").then(|| format!("{MACOS_SOURCE} ({} bytes), repeated", std::fs::metadata(MACOS_SOURCE).map(|m| m.len()).unwrap_or(0))), "bytes": std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0), "path": f.path.strip_prefix(root).unwrap_or(&f.path).display().to_string()})).collect::<Vec<_>>(),
    });
    let out = o.out.clone().unwrap_or_else(|| target_dir(root).join("bench").join(format!("bench-{mode}.json")));
    if o.strata {
        results["strata"] = strata::run(root, &o, &fixtures)?;
        // kept even if the other tools' part is stopped (memgate kills a job on low memory)
        save(&results, &out)?;
    }
    if o.others {
        results["others"] = others::run(&o, &fixtures);
        save(&results, &out)?;
    }
    if o.apps_on {
        // one app at a time: each is quit (and its processes gone) before the next starts
        results["apps"] = crate::bench_apps::run(root, &o, &fixtures);
    }
    results["load_after"] = load();
    let md = save(&results, &out)?;
    println!("{md}");
    eprintln!("bench: wrote {} and {}", out.display(), out.with_extension("md").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_of_odd_and_even_counts() {
        assert_eq!(summary(&[3.0, 1.0, 2.0])["median"], 2.0);
        assert_eq!(summary(&[4.0, 1.0, 2.0, 3.0])["median"], 2.5);
        assert_eq!(summary(&[5.0, 1.0])["min"], 1.0);
        assert_eq!(summary(&[]), Value::Null);
        assert_eq!(summary(&[f64::NAN, 2.0])["n"], 1);
    }

    #[test]
    fn parts_merge_file_by_file() {
        let mut a = json!({"runs": 3, "strata": {"files": [{"format": "jsonl", "size": "1G", "lines": 1}], "launch": {"x": 1}}});
        merge(
            &mut a,
            json!({"runs": 9, "strata": {"files": [{"format": "jsonl", "size": "5G", "lines": 5}, {"format": "jsonl", "size": "1G", "lines": 2}]}}),
        );
        assert_eq!(a["runs"], 9);
        assert_eq!(a["strata"]["launch"]["x"], 1);
        let files = a["strata"]["files"].as_array().unwrap();
        assert_eq!((files.len(), files[0]["lines"].clone(), files[1]["size"].clone()), (2, json!(2), json!("5G")));
        let mut w = json!({"idle": {"by": {"input": 63}, "cpu": 1}});
        merge(&mut w, json!({"idle": {"by": {"autosave": 2}}}));
        assert_eq!(w, json!({"idle": {"by": {"autosave": 2}, "cpu": 1}}));
        let mut s = json!({"median": 1.0, "samples": [1.0]});
        merge(&mut s, json!({"median": 2.0, "samples": [2.0, 2.0]}));
        assert_eq!(s["samples"], json!([2.0, 2.0]));
    }

    #[test]
    fn sizes_and_options_parse() {
        assert_eq!(size_of("100M"), Ok(100 << 20));
        assert_eq!(size_of("5G"), Ok(5 << 30));
        assert!(size_of("5").is_err() && size_of("0M").is_err());
        let o = options(&["--quick".into(), "--sizes".into(), "1G".into(), "--only".into(), "strata".into()]).unwrap();
        assert_eq!((o.runs, o.sizes.len(), o.others, o.small), (3, 1, false, false));
        assert!(options(&["--formats".into(), "xml".into()]).is_err());
    }
}
