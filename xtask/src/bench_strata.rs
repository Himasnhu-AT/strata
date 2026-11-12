//! Strata's side of `cargo xtask bench`: a release build in a real window, driven over its control
//! port. Every timing comes from the app (ready file, `app.stats` job seconds and frame CPU) or is
//! bounded by it (`ui.wait` is answered on the frame where the element first appears).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ashlar_control::Client;
use serde_json::{Value, json};

use crate::bench::{Fixture, Options, summary};

/// Kills the child when dropped, whatever happens in between.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running Strata window with a control connection.
pub struct Strata {
    child: Running,
    client: Client,
    pid: u32,
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bench: no free port: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// A fresh, empty folder (removed first when it exists).
fn fresh(dir: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("bench: {}: {e}", dir.display()))?;
    Ok(dir.to_path_buf())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Launch `exe` with `home` as its HOME (settings, recent files and autosave stay in there) and
/// wait for its first presented frame: (app, the app's own launch → present ms, spawn → ready file ms).
fn launch(exe: &Path, home: &Path) -> Result<(Strata, f64, f64), String> {
    let keys = home.join("keys");
    std::fs::create_dir_all(&keys).map_err(|e| format!("bench: {}: {e}", keys.display()))?;
    let ready = home.join("ready.json");
    let _ = std::fs::remove_file(&ready);
    let port = free_port()?;
    let stderr = std::fs::File::create(home.join("stderr.txt")).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    let started = Instant::now();
    let child = Command::new(exe)
        .args(["--control", &port.to_string()])
        .env("HOME", home)
        .env(ashlar_control::key::KEY_DIR_ENV, &keys)
        .env("STRATA_READY_FILE", &ready)
        // no STRATA_QUIET: the window comes to the front as it does for a person, because
        // ashlar runs no UI pass for a covered window (only for pending debugger requests)
        .env("STRATA_LOG", "warn")
        .env("SMTP_USER", "")
        .env("SMTP_PASS", "")
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("bench: {}: {e}", exe.display()))?;
    let pid = child.id();
    let mut child = Running(child);
    let deadline = started + Duration::from_secs(60);
    let (ready_ms, spawn_ms) = loop {
        if let Ok(text) = std::fs::read_to_string(&ready)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(r) = v["ready_ms"].as_f64()
        {
            break (r, ms(started.elapsed()));
        }
        if let Ok(Some(status)) = child.0.try_wait() {
            return Err(format!("bench: strata exited before its first frame ({status}); see {}", home.join("stderr.txt").display()));
        }
        if Instant::now() > deadline {
            return Err("bench: strata wrote no ready file within 60 s".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut tries = 0;
    let client = loop {
        match Client::connect(port, &keys) {
            Ok(c) => break c,
            Err(e) if tries > 200 => return Err(format!("bench: control port: {e}")),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    Ok((Strata { child, client, pid }, ready_ms, spawn_ms))
}

impl Strata {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.client.call(method, params).map_err(|e| format!("bench: {method}: {e}"))
    }

    fn exec(&mut self, command: &str, params: Value) -> Result<Value, String> {
        self.call("engine.execute", json!({"command": command, "params": params}))
    }

    fn stats(&mut self, reset: bool) -> Result<Value, String> {
        self.call("app.stats", if reset { json!({"reset": true}) } else { json!({}) })
    }

    /// The text of an element (`None` when it is not on screen).
    fn text(&mut self, id: &str) -> Option<String> {
        self.call("ui.get", json!({"target": {"id": id}})).ok().and_then(|v| v["value"].as_str().map(str::to_string))
    }

    /// Resident set size from `ps`, MB.
    fn rss_mb(&self) -> Option<f64> {
        crate::bench::output("ps", &["-o", "rss=", "-p", &self.pid.to_string()]).and_then(|s| s.parse::<f64>().ok()).map(|kb| kb / 1024.0)
    }

    /// Quit without saving and wait for the process to end (killed after 5 s).
    fn quit(mut self) {
        let _ = self.call("app.quit", json!({"discard": true}));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Poll `log.files` until `file` is indexed: ms since `since`.
    fn wait_indexed(&mut self, file: u64, since: Instant, limit: Duration) -> Result<(f64, u64), String> {
        loop {
            let v = self.exec("log.files", json!({}))?;
            let f = v["files"].as_array().and_then(|fs| fs.iter().find(|f| f["file"].as_u64() == Some(file))).cloned().unwrap_or(Value::Null);
            match f["indexing"]["state"].as_str() {
                Some("complete") => return Ok((ms(since.elapsed()), f["lines"].as_u64().unwrap_or(0))),
                Some("failed") => return Err(format!("bench: indexing failed: {}", f["indexing"]["error"])),
                _ => {}
            }
            if since.elapsed() > limit {
                return Err(format!("bench: not indexed after {} s", limit.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Wait for the first job of `kind` newer than `after` to finish: (its id, its seconds, the
    /// wall ms since `since`).
    fn wait_job(&mut self, kind: &str, after: u64, since: Instant, limit: Duration) -> Result<(u64, f64, f64), String> {
        loop {
            let v = self.stats(false)?;
            let found = v["jobs"]["recent"]
                .as_array()
                .and_then(|r| r.iter().find(|j| j["kind"] == kind && j["id"].as_u64().is_some_and(|id| id > after)).cloned());
            if let Some(j) = found {
                if j["status"] != "done" {
                    return Err(format!("bench: job {kind} {}: {}", j["status"], j["message"]));
                }
                return Ok((j["id"].as_u64().unwrap_or(after), j["seconds"].as_f64().unwrap_or(f64::NAN) * 1000.0, ms(since.elapsed())));
            }
            if since.elapsed() > limit {
                return Err(format!("bench: no {kind} job finished within {} s", limit.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// The newest job id so far.
    fn last_job(&mut self) -> Result<u64, String> {
        let v = self.stats(false)?;
        let ids = |k: &str| v["jobs"][k].as_array().map(|a| a.iter().filter_map(|j| j["id"].as_u64()).max().unwrap_or(0)).unwrap_or(0);
        Ok(ids("recent").max(ids("active")))
    }

    /// Wait until no job runs (histogram, summaries) so the next measurement starts quiet. The
    /// `ui.wait` keeps frames coming even when the window got covered, so the view still starts
    /// the jobs a command asked for.
    fn wait_quiet(&mut self, limit: Duration) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < limit {
            // the control server answers any request held over 60 s with a timeout: wait in
            // slices (a slice that times out is fine, the loop asks again)
            let _ = self.call("ui.wait", json!({"for": "idle", "timeout_ms": 30_000}));
            let v = self.stats(false)?;
            if v["jobs"]["active"].as_array().is_none_or(Vec::is_empty) {
                return Ok(());
            }
        }
        Err("bench: jobs still running".into())
    }
}

fn mb(v: &Value) -> Option<f64> {
    v.as_f64().map(|b| b / f64::from(1u32 << 20))
}

/// The first number in `text`, thousands separators ignored ("1,234 matches" → 1234).
fn number_in(text: &str) -> Option<u64> {
    let digits: String =
        text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == ',').filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Build Strata (release, or `release-small` defined on the command line so the workspace's
/// profiles stay untouched) and return its executable.
fn build(root: &Path, small: bool) -> Result<PathBuf, String> {
    let mut args = vec!["build", "-p", "strata", "--bin", "strata", "--locked", "--message-format=json-render-diagnostics"];
    // ashlar's release-small: whole-program LTO, one codegen unit, size-optimised, no symbols
    let small_cfg = [
        "--profile",
        "release-small",
        "--config",
        "profile.release-small.inherits=\"release\"",
        "--config",
        "profile.release-small.opt-level=\"s\"",
        "--config",
        "profile.release-small.lto=\"fat\"",
        "--config",
        "profile.release-small.codegen-units=1",
        "--config",
        "profile.release-small.debug=false",
        "--config",
        "profile.release-small.strip=true",
    ];
    if small {
        args.extend(small_cfg);
    } else {
        args.push("--release");
    }
    eprintln!("bench: cargo {}", args.join(" "));
    let out = Command::new(crate::cargo())
        .current_dir(root)
        .args(&args)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("bench: cargo build: {e}"))?;
    if !out.status.success() {
        return Err("bench: building strata failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| {
            m["reason"] == "compiler-artifact"
                && m["target"]["name"] == "strata"
                && m["target"]["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin"))
        })
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "bench: cargo reported no strata executable".into())
}

fn file_len(p: &Path) -> Option<u64> {
    std::fs::metadata(p).ok().map(|m| m.len())
}

/// Release size as built, stripped, and release-small.
fn binary(root: &Path, exe: &Path, dir: &Path, small: bool) -> Value {
    let stripped = dir.join("strata-stripped");
    let _ = std::fs::remove_file(&stripped);
    let ok = Command::new("strip").arg("-o").arg(&stripped).arg(exe).status().is_ok_and(|s| s.success());
    let small_bytes = if small {
        match build(root, true) {
            Ok(p) => file_len(&p),
            Err(e) => {
                eprintln!("{e}");
                None
            }
        }
    } else {
        None
    };
    json!({
        "release_bytes": file_len(exe),
        "release_stripped_bytes": if ok { file_len(&stripped) } else { None },
        "release_small_bytes": small_bytes,
        "method": "file size of target/<profile>/strata; stripped = `strip -o` of the release binary; release-small = opt-level s, fat LTO, 1 codegen unit, strip (ashlar's profile, passed with --config)",
    })
}

/// Launch to first frame, warm and cold-ish, and the empty window's idle memory.
fn launches(exe: &Path, dir: &Path, runs: usize) -> Result<Value, String> {
    let home = fresh(&dir.join("home-warm"))?;
    // one unmeasured launch: settings exist, the binary and shader caches are warm
    let (app, _, _) = launch(exe, &home)?;
    app.quit();
    let (mut ready, mut spawn, mut footprint, mut rss) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..runs {
        eprintln!("bench: warm launch {}/{runs}", i + 1);
        let (mut app, r, s) = launch(exe, &home)?;
        ready.push(r);
        spawn.push(s);
        std::thread::sleep(Duration::from_secs(2));
        let st = app.stats(false)?;
        if let Some(f) = mb(&st["process"]["footprint_bytes"]) {
            footprint.push(f);
        }
        if let Some(r) = app.rss_mb() {
            rss.push(r);
        }
        app.quit();
        std::thread::sleep(Duration::from_millis(300));
    }
    let (mut cold_ready, mut cold_spawn) = (Vec::new(), Vec::new());
    for i in 0..runs.min(5) {
        eprintln!("bench: cold-ish launch {}/{}", i + 1, runs.min(5));
        let d = fresh(&dir.join(format!("cold-{i}")))?;
        let copy = d.join("strata");
        std::fs::copy(exe, &copy).map_err(|e| format!("bench: copy strata: {e}"))?;
        let (app, r, s) = launch(&copy, &fresh(&d.join("home"))?)?;
        cold_ready.push(r);
        cold_spawn.push(s);
        app.quit();
        let _ = std::fs::remove_dir_all(&d);
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(json!({
        "warm": {"launch_to_present_ms": summary(&ready), "spawn_to_ready_ms": summary(&spawn)},
        "cold_ish": {"launch_to_present_ms": summary(&cold_ready), "spawn_to_ready_ms": summary(&cold_spawn)},
        "idle_empty_window": {"footprint_mb": summary(&footprint), "rss_mb": summary(&rss), "after_s": 2},
        "method": "launch_to_present: the app's own figure (first line of run() to the first presented frame, STRATA_READY_FILE). spawn_to_ready: process spawn to the ready file, so exec, dyld and main count too. warm: same binary and HOME as an unmeasured first launch. cold-ish: the binary copied to a new path and an empty HOME each run (first-run work of a new binary: code-signature checks, dyld and Metal shader caches miss); the disk cache is warm (no purge without root). The window opens in front, as for a person.",
    }))
}

/// The searches for one format: (name, command, params, job kind, element with the count).
fn searches(format: &str) -> Vec<(&'static str, &'static str, Value, &'static str, &'static str)> {
    let q = crate::bench::queries(format);
    vec![
        ("literal", "search.run", json!({"query": q.literal}), "strata.search", "strata.toolbar.matches"),
        ("regex", "search.run", json!({"query": q.regex, "regex": true}), "strata.search", "strata.toolbar.matches"),
        ("field", "view.filter", json!({"filter": q.field}), "strata.filter", "strata.status.filters"),
    ]
}

/// The query text of each search, for the report and the other tools.
pub fn search_queries(format: &str) -> Vec<(&'static str, String)> {
    searches(format)
        .into_iter()
        .map(|(name, _, p, _, _)| (name, p["query"].as_str().or(p["filter"].as_str()).unwrap_or_default().to_string()))
        .collect()
}

/// Open, index, memory and searches of one file, `runs` times (a new process each run).
fn file_runs(exe: &Path, dir: &Path, f: &Fixture, runs: usize) -> Result<Value, String> {
    let limit = Duration::from_secs(600);
    let mut first = Vec::new();
    let mut first_lines_shown = Vec::new();
    let (mut indexed, mut index_job) = (Vec::new(), Vec::new());
    let (mut fp_open, mut rss_open, mut peak_search, mut rss_search) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut search_job: Vec<(String, Vec<f64>, Vec<f64>, Option<u64>)> =
        searches(&f.format).iter().map(|s| (s.0.to_string(), Vec::new(), Vec::new(), None)).collect();
    let mut lines = 0;
    for i in 0..runs {
        eprintln!("bench: {} {}: run {}/{runs}", f.format, f.label, i + 1);
        let (mut app, _, _) = launch(exe, &fresh(&dir.join("home-file"))?)?;
        let t0 = Instant::now();
        let opened = app.exec("log.open_dialog", json!({"path": f.path.display().to_string()}))?;
        let file = opened["file"].as_u64().ok_or("bench: log.open_dialog returned no file id")?;
        app.call("ui.wait", json!({"for": "element", "target": {"id": "strata.table.row.0"}, "timeout_ms": 50_000}))?;
        first.push(ms(t0.elapsed()));
        if let Some(n) = app.text("strata.status.lines").as_deref().and_then(number_in) {
            first_lines_shown.push(n as f64);
        }
        let (done_ms, n) = app.wait_indexed(file, t0, limit)?;
        indexed.push(done_ms);
        lines = n;
        // the index job finishes its chunk summaries after the index itself is complete
        app.wait_quiet(limit)?;
        let st = app.stats(false)?;
        let job_ms: f64 = st["jobs"]["recent"]
            .as_array()
            .map(|r| {
                r.iter().filter(|j| j["kind"] == "strata.index" || j["kind"] == "strata.open").filter_map(|j| j["seconds"].as_f64()).sum::<f64>()
                    * 1000.0
            })
            .unwrap_or(f64::NAN);
        index_job.push(if job_ms > 0.0 { job_ms } else { f64::NAN });
        std::thread::sleep(Duration::from_secs(1));
        let st = app.stats(false)?;
        if let Some(v) = mb(&st["process"]["footprint_bytes"]) {
            fp_open.push(v);
        }
        if let Some(v) = app.rss_mb() {
            rss_open.push(v);
        }
        for (k, (name, cmd, params, kind, counter)) in searches(&f.format).into_iter().enumerate() {
            let after = app.last_job()?;
            let mut p = params.clone();
            p["file"] = json!(file);
            let t = Instant::now();
            app.exec(cmd, p)?;
            app.wait_quiet(limit)?;
            let (_, job, wall) = app.wait_job(kind, after, t, limit)?;
            // the counter is drawn on the next frame
            app.call("ui.wait", json!({"for": "element", "target": {"id": counter}, "timeout_ms": 5_000})).ok();
            let count =
                app.text(counter).as_deref().and_then(|t| if name == "field" { t.split('·').nth(1).and_then(number_in) } else { number_in(t) });
            if let Some(s) = search_job.get_mut(k) {
                s.1.push(job);
                s.2.push(wall);
                s.3 = count.or(s.3);
            }
            app.exec(if cmd == "view.filter" { "view.clear_filters" } else { "search.clear" }, json!({"file": file}))?;
            app.wait_quiet(limit)?;
        }
        let st = app.stats(false)?;
        if let Some(v) = mb(&st["process"]["peak_bytes"]) {
            peak_search.push(v);
        }
        if let Some(v) = app.rss_mb() {
            rss_search.push(v);
        }
        app.quit();
        std::thread::sleep(Duration::from_millis(300));
    }
    let bytes = file_len(&f.path).unwrap_or(f.bytes) as f64;
    let gbps: Vec<f64> = index_job.iter().map(|ms| bytes / (ms / 1000.0) / 1e9).collect();
    let queries = search_queries(&f.format);
    let search: serde_json::Map<String, Value> = search_job
        .into_iter()
        .zip(queries)
        .map(|((name, job, _, count), (_, q))| {
            let mbps: Vec<f64> = job.iter().map(|ms| bytes / (ms / 1000.0) / 1e6).collect();
            (name, json!({"query": q, "job_ms": summary(&job), "mb_per_s": summary(&mbps), "count": count}))
        })
        .collect();
    Ok(json!({
        "format": f.format,
        "size": f.label,
        "bytes": bytes,
        "lines": lines,
        "open": {
            "first_lines_ms": summary(&first),
            "lines_indexed_at_first_lines": summary(&first_lines_shown),
            "indexed_ms": summary(&indexed),
            "index_job_ms": summary(&index_job),
            "index_job_gb_per_s": summary(&gbps),
        },
        "memory": {
            "footprint_after_open_mb": summary(&fp_open),
            "rss_after_open_mb": summary(&rss_open),
            "peak_footprint_after_searches_mb": summary(&peak_search),
            "rss_after_searches_mb": summary(&rss_search),
        },
        "search": search,
    }))
}

/// Frame CPU while scrolling the table: small wheel steps (rows already parsed nearby) and big
/// jumps (every frame parses rows it has not seen).
fn scroll(exe: &Path, dir: &Path, f: &Fixture) -> Result<Value, String> {
    let (mut app, _, _) = launch(exe, &fresh(&dir.join("home-scroll"))?)?;
    let opened = app.exec("log.open_dialog", json!({"path": f.path.display().to_string()}))?;
    let file = opened["file"].as_u64().ok_or("bench: no file id")?;
    app.wait_indexed(file, Instant::now(), Duration::from_secs(600))?;
    app.wait_quiet(Duration::from_secs(600))?;
    let mut out = serde_json::Map::new();
    for (name, steps, dy) in [("wheel", 480u32, 36.0), ("jump", 120, 20_000.0)] {
        app.stats(true)?;
        let before = app.stats(false)?;
        let t = Instant::now();
        for i in 0..steps {
            let dy = if i < steps / 2 { dy } else { -dy };
            app.call("ui.scroll", json!({"target": {"id": "strata.table"}, "dy": dy}))?;
        }
        let wall = ms(t.elapsed());
        let after = app.stats(false)?;
        let frames = after["frames"].as_u64().unwrap_or(0).saturating_sub(before["frames"].as_u64().unwrap_or(0));
        out.insert(
            name.into(),
            json!({
                "steps": steps,
                "dy_pt": dy,
                "frames": frames,
                // 0 painted frames: the window was covered, ashlar ran hidden passes only
                "painted_frames": after["frame_cpu"]["window"],
                "frame_cpu_p50_ms": after["frame_cpu"]["p50_ms"],
                "frame_cpu_p95_ms": after["frame_cpu"]["p95_ms"],
                "frame_cpu_max_ms": after["frame_cpu"]["max_ms"],
                "ui_p50_ms": after["p50_ms"],
                "ui_p95_ms": after["p95_ms"],
                "wall_ms": wall,
            }),
        );
    }
    app.quit();
    out.insert("file".into(), json!(format!("{} {}", f.format, f.label)));
    out.insert("method".into(), json!("ui.scroll over the table (a wheel event at its center, one per request, reversing halfway); app.stats reset before each kind, then frame_cpu (eframe's whole-frame CPU on the UI thread, painting included) and ui (the runtime's view code) percentiles over the frames of the scroll"));
    Ok(Value::Object(out))
}

/// Frames and CPU while a window sits idle: empty, and following an unchanged file.
fn idle(exe: &Path, dir: &Path, f: &Fixture, secs: u64) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for case in ["empty", "following"] {
        eprintln!("bench: idle {case}, {secs} s");
        let (mut app, _, _) = launch(exe, &fresh(&dir.join("home-idle"))?)?;
        if case == "following" {
            let opened = app.exec("log.open_dialog", json!({"path": f.path.display().to_string(), "follow": true}))?;
            let file = opened["file"].as_u64().ok_or("bench: no file id")?;
            app.wait_indexed(file, Instant::now(), Duration::from_secs(600))?;
            app.wait_quiet(Duration::from_secs(600))?;
        }
        // the settings written once after an open (recent files) are due a few seconds later
        std::thread::sleep(Duration::from_secs(6));
        let a = app.stats(false)?;
        std::thread::sleep(Duration::from_secs(secs));
        let b = app.stats(false)?;
        let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
        let frames = n(&b, "frames").saturating_sub(n(&a, "frames"));
        let by = |v: &Value, r: &str| v["wakes"]["frames_by"][r].as_u64().unwrap_or(0);
        // the second app.stats is a control request: it wakes the window once itself
        let probe = by(&b, "control").saturating_sub(by(&a, "control"));
        let cpu = b["process"]["cpu_s"].as_f64().unwrap_or(0.0) - a["process"]["cpu_s"].as_f64().unwrap_or(0.0);
        let mut reasons = serde_json::Map::new();
        if let Some(m) = b["wakes"]["frames_by"].as_object() {
            for (k, v) in m {
                let d = v.as_u64().unwrap_or(0).saturating_sub(by(&a, k));
                if d > 0 {
                    reasons.insert(k.clone(), json!(d));
                }
            }
        }
        out.insert(
            case.into(),
            json!({
                "seconds": secs,
                "frames": frames,
                "frames_excluding_probe": frames.saturating_sub(probe),
                "frames_by_reason": reasons,
                "process_cpu_s": (cpu * 1e4).round() / 1e4,
                "cpu_percent": (cpu / secs as f64 * 100.0 * 1e3).round() / 1e3,
            }),
        );
        app.quit();
    }
    out.insert("file".into(), json!(format!("{} {}", f.format, f.label)));
    out.insert("method".into(), json!("app.stats before and after the idle period: frames drawn (minus the one the second app.stats request wakes), wake reasons, and process CPU time of all threads (watcher included)"));
    Ok(Value::Object(out))
}

/// strata-core alone, in this process: the line index and the chunk summaries, no window.
fn core_index(f: &Fixture, runs: usize) -> Result<Value, String> {
    if cfg!(debug_assertions) {
        return Ok(
            json!({"file": format!("{} {}", f.format, f.label), "skipped": "xtask was built without optimizations: run `cargo run --release -p xtask -- bench` for these"}),
        );
    }
    let (mut index, mut summaries) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        let t = Instant::now();
        let log = strata_core::LogFile::open(strata_core::FileId(1), &f.path).map_err(|e| format!("bench: core open: {e}"))?;
        log.index_all(8 << 20, &|| false, &mut |_, _| {}).map_err(|e| format!("bench: core index: {e}"))?;
        index.push(ms(t.elapsed()));
        let t = Instant::now();
        let chunks = log.with_index(|ix| ix.chunks());
        for c in 0..chunks {
            log.summarize_chunk(c, &|| false).map_err(|e| format!("bench: summarize: {e}"))?;
        }
        summaries.push(ms(t.elapsed()));
    }
    let bytes = file_len(&f.path).unwrap_or(f.bytes) as f64;
    let rate = |v: &[f64]| summary(&v.iter().map(|ms| bytes / (ms / 1000.0) / 1e9).collect::<Vec<_>>());
    Ok(json!({
        "file": format!("{} {}", f.format, f.label),
        "open_and_index_ms": summary(&index),
        "index_gb_per_s": rate(&index),
        "summaries_ms": summary(&summaries),
        "summaries_gb_per_s": rate(&summaries),
    }))
}

pub fn run(root: &Path, o: &Options, fixtures: &[Fixture]) -> Result<Value, String> {
    let exe = build(root, false)?;
    let dir = fresh(&crate::bench::target_dir(root).join("bench").join("strata"))?;
    let mut out = serde_json::Map::new();
    out.insert("binary".into(), binary(root, &exe, &dir, o.small));
    out.insert("launch".into(), launches(&exe, &dir, o.runs)?);
    let mut files = Vec::new();
    let mut core = Vec::new();
    for f in fixtures {
        files.push(file_runs(&exe, &dir, f, o.runs)?);
        core.push(core_index(f, o.runs)?);
    }
    out.insert("files".into(), json!(files));
    out.insert("core".into(), json!(core));
    out.insert(
        "method".into(),
        json!("A new process with an empty HOME per run. first_lines: log.open_dialog sent → ui.wait reply for the first table row's element (answered on the frame that drew it). indexed: until log.files reports the file complete (polled every 2 ms). index_job: the app's own seconds for its open + index jobs (line index and per-chunk time/level summaries). search job_ms: the app's seconds for the search or filter job over the whole indexed file (count = the toolbar's match counter or the status bar's shown lines). Memory: macOS physical footprint from app.stats and RSS from ps; peak = the OS's lifetime peak footprint."),
    );
    // scroll and idle on the biggest jsonl file up to 1 GB, the size PLAN section 6 names
    let pick = fixtures.iter().filter(|f| f.format == "jsonl" && f.bytes <= 1 << 30).max_by_key(|f| f.bytes).or_else(|| fixtures.first());
    if let Some(f) = pick {
        out.insert("scroll".into(), scroll(&exe, &dir, f)?);
        out.insert("idle".into(), idle(&exe, &dir, f, o.idle_secs)?);
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_read_from_labels() {
        assert_eq!(number_in("1,234 matches"), Some(1234));
        assert_eq!(number_in("1 of 2"), Some(1));
        assert_eq!(number_in("no matches"), None);
        assert_eq!(" 89,321 shown".split('·').next().and_then(number_in), Some(89_321));
    }

    #[test]
    fn every_format_has_three_searches() {
        for f in ["jsonl", "plain"] {
            let q = search_queries(f);
            assert_eq!(q.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["literal", "regex", "field"]);
            assert!(q.iter().all(|(_, t)| !t.is_empty()));
        }
    }
}
