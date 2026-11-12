//! The other tools in `cargo xtask bench`, measured from the outside on the same fixtures.
//!
//! - CLI baselines (`wc -l`, `grep`, `rg`): wall time of the process (median of N) and its maximum
//!   RSS from `/usr/bin/time -l`. They read the file once and exit: no window, no index kept.
//! - `less`: run in a pseudo-terminal (`script -q /dev/null less -S FILE`): time until the first line
//!   of the file is written to the terminal, and after `G` until the last line is.
//! - Apps with a window, and lnav: `bench_apps.rs`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::bench::{Fixture, Options, summary};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// An executable on PATH (real files only: shell functions and aliases do not count).
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|c| c.is_file()))
}

/// Run `cmd` under `/usr/bin/time -l`: (wall ms, max RSS MB, stdout).
fn timed(cmd: &Path, args: &[&str]) -> Result<(f64, Option<f64>, String), String> {
    let t = Instant::now();
    let out =
        Command::new("/usr/bin/time").arg("-l").arg(cmd).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{}: {e}", cmd.display()))?;
    let wall = ms(t.elapsed());
    // grep exits 1 when nothing matches; that is still a measurement
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(format!("{} failed ({})", cmd.display(), out.status));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let rss = err
        .lines()
        .find(|l| l.contains("maximum resident set size"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse::<f64>().ok())
        .map(|b| b / f64::from(1u32 << 20));
    Ok((wall, rss, String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// N timed runs of one command (after one unmeasured run, so the file is in the page cache).
/// `runs == 1`: no unmeasured run (the multi-gigabyte files, where grep takes minutes).
fn cli_case(cmd: &Path, args: &[&str], runs: usize) -> Value {
    if runs > 1 {
        let _ = timed(cmd, args);
    }
    let (mut wall, mut rss, mut out) = (Vec::new(), Vec::new(), String::new());
    for _ in 0..runs {
        match timed(cmd, args) {
            Ok((w, r, o)) => {
                wall.push(w);
                rss.extend(r);
                out = o;
            }
            Err(e) => return json!({"error": e}),
        }
    }
    json!({"command": format!("{} {}", cmd.display(), args.join(" ")), "wall_ms": summary(&wall), "max_rss_mb": summary(&rss), "output": out})
}

fn cli_tools(f: &Fixture, runs: usize) -> Value {
    // BSD grep reads about 50–150 MB/s: one run on files over 1 GB
    let runs = if f.bytes > 1 << 30 { 1 } else { runs };
    let path = f.path.display().to_string();
    let p = path.as_str();
    let queries = crate::bench_strata::search_queries(&f.format);
    let q = |name: &str| queries.iter().find(|(n, _)| *n == name).map(|(_, q)| q.clone()).unwrap_or_default();
    let (literal, regex) = (q("literal"), q("regex"));
    let field = crate::bench::queries(&f.format).grep_field;
    let mut out = serde_json::Map::new();
    out.insert("wc_l".into(), cli_case(Path::new("/usr/bin/wc"), &["-l", p], runs));
    let grep = Path::new("/usr/bin/grep");
    out.insert("grep_literal".into(), cli_case(grep, &["-c", "-i", "-F", &literal, p], runs));
    out.insert("grep_regex".into(), cli_case(grep, &["-c", "-E", &regex, p], runs));
    out.insert("grep_field".into(), cli_case(grep, &["-c", "-E", field, p], runs));
    match on_path("rg") {
        Some(rg) => {
            out.insert("rg_literal".into(), cli_case(&rg, &["-c", "-i", "-F", &literal, p], runs));
            out.insert("rg_regex".into(), cli_case(&rg, &["-c", &regex, p], runs));
            out.insert("rg_field".into(), cli_case(&rg, &["-c", field, p], runs));
        }
        None => {
            out.insert(
                "rg".into(),
                json!({"installed": false, "would_run": format!("rg -c -i -F {literal} FILE; rg -c '{regex}' FILE; rg -c '{field}' FILE")}),
            );
        }
    }
    json!({"file": format!("{} {}", f.format, f.label), "results": out})
}

/// The last line of a file (for `less`'s jump to the end).
fn last_line(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // the last log line, not a stack-trace line under it (those repeat)
    text.trim_end().lines().rev().find(|l| l.starts_with('{') || l.starts_with('[')).map(str::to_string)
}

/// `less` in a pseudo-terminal: ms to the first screen, and to the last line after `G`.
fn less_once(p: &Path, first: &str, last: &str) -> Result<(f64, f64), String> {
    let t = Instant::now();
    let mut child = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/usr/bin/less", "-S"])
        .arg(p)
        .env("LESS", "")
        .env("LESSOPEN", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("less: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("less: no stdin")?;
    let mut stdout = child.stdout.take().ok_or("less: no stdout")?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf.get(..n).unwrap_or_default().to_vec()).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let mut wait_for = |needle: &str, limit: Duration| -> Result<f64, String> {
        let deadline = Instant::now() + limit;
        loop {
            if String::from_utf8_lossy(&seen).contains(needle) {
                return Ok(ms(t.elapsed()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("less: `{needle}` not shown within {} s", limit.as_secs()));
            }
            match rx.recv_timeout(left) {
                Ok(b) => seen.extend(b),
                Err(_) => return Err("less: no output".into()),
            }
        }
    };
    let result = (|| {
        let first_ms = wait_for(first, Duration::from_secs(30))?;
        // only what comes after the jump counts for the end
        stdin.write_all(b"G").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        let end_ms = wait_for(last, Duration::from_secs(300))?;
        Ok((first_ms, end_ms))
    })();
    let _ = stdin.write_all(b"q");
    let _ = stdin.flush();
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return result;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn less(f: &Fixture, runs: usize) -> Value {
    let Some(last) = last_line(&f.path) else { return json!({"error": "cannot read the file"}) };
    // the start of each line, timestamp included (unique per line); `less -S` does not wrap,
    // and 30 characters fit the pseudo-terminal's width
    let piece = |l: &str| l.chars().take(30).collect::<String>();
    let first = std::fs::File::open(&f.path).ok().and_then(|mut h| {
        let mut b = vec![0u8; 512];
        let n = h.read(&mut b).ok()?;
        String::from_utf8_lossy(b.get(..n)?).lines().next().map(piece)
    });
    let Some(first) = first else { return json!({"error": "cannot read the file"}) };
    let last = piece(&last);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        match less_once(&f.path, &first, &last) {
            Ok((x, y)) => {
                a.push(x);
                b.push(y);
            }
            Err(e) => return json!({"error": e}),
        }
    }
    json!({"file": format!("{} {}", f.format, f.label), "first_screen_ms": summary(&a), "end_after_g_ms": summary(&b), "version": crate::bench::output("/usr/bin/less", &["--version"]).and_then(|v| v.lines().next().map(str::to_string))})
}

pub fn run(o: &Options, fixtures: &[Fixture]) -> Value {
    let cli: Vec<Value> = fixtures.iter().map(|f| cli_tools(f, o.runs)).collect();
    let less: Vec<Value> = fixtures.iter().map(|f| less(f, o.runs)).collect();
    json!({
        "cli": cli,
        "less": less,
        "versions": {
            "grep": crate::bench::output("/usr/bin/grep", &["--version"]).and_then(|v| v.lines().next().map(str::to_string)),
            "rg": on_path("rg").and_then(|p| crate::bench::output(&p.display().to_string(), &["--version"])).and_then(|v| v.lines().next().map(str::to_string)),
        },
        "method": "CLI: wall time of the process and /usr/bin/time -l's maximum RSS, after one unmeasured run (page cache warm); default locale. less: a pseudo-terminal from script(1); ms until the file's first line is written, and after G until its last line is.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tools_are_not_found() {
        assert!(on_path("surely-not-a-real-tool-xyz").is_none());
    }
}
