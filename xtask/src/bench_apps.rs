//! Log viewers with a window (and lnav, headless), measured from the outside, strictly one
//! app at a time, on the same fixtures. Strata itself is measured this way too, so its row and
//! the others' rows were taken by the same method.
//!
//! - Launch to window: the app is started without a file; a JXA script polls the window server
//!   (`CGWindowListCopyWindowInfo`, on-screen, layer 0, every ~5 ms plus the call's own cost) and
//!   reports when the app's first window is on screen.
//! - Open to a window showing the file: the same, started with the file, until an on-screen
//!   window of the app has the file's name in its title. That is when the app has opened the file
//!   and drawn a window for it, the closest outside proxy for "first lines on screen".
//! - Fully loaded ("settled"): `ps` every 50 ms over the app's processes; the first whole second
//!   in which they used under 20 ms of CPU. An upper bound of "fully indexed / usable".
//! - RSS after open (at settled) and peak (the highest 50 ms sample), summed over the app's
//!   processes; idle CPU after open over the next seconds (first run of each file).
//! - Search: driven only where the app has a CLI for it (lnav headless SQL). The GUI apps' find
//!   boxes are not driven: synthetic keystrokes would measure the automation, not the app.
//!
//! - lnav: headless only (`lnav -n -c ';SELECT …' FILE` under `/usr/bin/time -l`): load and index
//!   the file (`count(*)`), and each search as a SQL query; every query loads the file again.
//!
//! An app that is already running is skipped (it would reuse its process); a run is stopped when
//! the Mac's available memory falls under 12 % or the app's RSS passes 8 GB, and says so.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::bench::{Fixture, Options, output, summary};

/// Waits for a window (on screen or not: see `on_screen`) of `argv[0]` (owner name) whose title contains `argv[1]`, for
/// `argv[2]` ms; prints the epoch ms it was seen and the title.
const WINDOW_JS: &str = r#"ObjC.import('CoreGraphics');
function run(argv) {
  const owner = argv[0], needle = argv[1] || '', limit = Number(argv[2] || 60000);
  const t0 = Date.now();
  while (Date.now() - t0 < limit) {
    const list = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionAll | $.kCGWindowListExcludeDesktopElements, 0))) || [];
    for (const w of list) {
      if (w.kCGWindowLayer === 0 && String(w.kCGWindowOwnerName || '') === owner && String(w.kCGWindowName || '').indexOf(needle) >= 0 && String(w.kCGWindowName || '') !== '') {
        return String(Date.now()) + ' ' + (w.kCGWindowIsOnscreen ? 'on' : 'off') + ' ' + String(w.kCGWindowName || '');
      }
    }
    delay(0.005);
  }
  return 'timeout';
}
"#;

const LOW_MEMORY_LEVEL: u64 = 12;
const MAX_RSS_MB: f64 = 8192.0;

fn epoch_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

fn memory_level() -> Option<u64> {
    output("sysctl", &["-n", "kern.memorystatus_level"]).and_then(|v| v.parse().ok())
}

/// `0:01.23` or `1:02:03.45` (ps `time`) in seconds.
fn ps_time(s: &str) -> Option<f64> {
    s.split(':').try_fold(0.0, |acc, part| part.trim().parse::<f64>().ok().map(|v| acc * 60.0 + v))
}

/// (CPU seconds, RSS MB, pids) of the processes whose executable path contains one of `pats`.
fn sample(pats: &[String]) -> (f64, f64, Vec<String>) {
    let text = output("ps", &["-A", "-o", "pid=,rss=,time=,comm="]).unwrap_or_default();
    let (mut cpu, mut rss, mut pids) = (0.0, 0.0, Vec::new());
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(r), Some(t)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let comm = parts.collect::<Vec<_>>().join(" ");
        if pats.iter().any(|p| comm.contains(p.as_str())) {
            cpu += ps_time(t).unwrap_or(0.0);
            rss += r.parse::<f64>().unwrap_or(0.0) / 1024.0;
            pids.push(pid.to_string());
        }
    }
    (cpu, rss, pids)
}

/// One app: how to start it (with or without a file), how to find and quit its processes.
struct App {
    key: &'static str,
    name: &'static str,
    /// Must exist for the app to count as installed.
    installed: PathBuf,
    /// The window server's owner name of its windows.
    owner: &'static str,
    processes: Vec<String>,
    launch: Box<dyn Fn(Option<&Path>, &Path) -> Command>,
    /// Quit with AppleScript (`tell application … to quit`), else SIGTERM.
    quit_name: Option<&'static str>,
    max_bytes: u64,
    /// Strata: started without a file, then `log.open_dialog {path}` over its control port
    /// (a log path on the command line is not opened in 0.1).
    control_open: bool,
}

fn apps(strata_bundle: Option<PathBuf>) -> Vec<App> {
    let mut v = Vec::new();
    if let Some(bundle) = strata_bundle {
        let b = bundle.clone();
        v.push(App {
            key: "strata",
            name: "Strata (same outside method)",
            installed: bundle.clone(),
            owner: "Strata",
            processes: vec!["Strata.app/Contents/MacOS/strata".into()],
            // through LaunchServices like the others: a process spawned from a background
            // parent is not activated and its window stays off screen
            launch: Box::new(move |_, home| {
                let port = std::net::TcpListener::bind(("127.0.0.1", 0)).ok().and_then(|l| l.local_addr().ok()).map_or(0, |a| a.port());
                let _ = std::fs::write(home.join("port"), port.to_string());
                let mut c = Command::new("/usr/bin/open");
                c.args(["-n", "-a"])
                    .arg(&b)
                    .arg("--env")
                    .arg(format!("HOME={}", home.display()))
                    .arg("--env")
                    .arg(format!("{}={}", ashlar_control::key::KEY_DIR_ENV, home.join("keys").display()))
                    .args(["--env", "STRATA_LOG=warn", "--env", "SMTP_USER=", "--env", "SMTP_PASS=", "--args", "--control", &port.to_string()]);
                c
            }),
            quit_name: None,
            max_bytes: u64::MAX,
            control_open: true,
        });
    }
    v.push(App {
        key: "console",
        name: "Console.app",
        installed: "/System/Applications/Utilities/Console.app".into(),
        owner: "Console",
        processes: vec!["Console.app/Contents/MacOS/Console".into()],
        launch: Box::new(|file, _| {
            let mut c = Command::new("/usr/bin/open");
            c.args(["-n", "-a", "Console"]).args(file);
            c
        }),
        quit_name: Some("Console"),
        max_bytes: 1 << 30,
        control_open: false,
    });
    v.push(App {
        key: "klogg",
        name: "klogg",
        installed: "/Applications/klogg.app".into(),
        owner: "klogg log viewer",
        processes: vec!["klogg.app/Contents/MacOS/klogg".into()],
        // started directly, its window stays off screen (never activated): open it as Finder does
        launch: Box::new(|file, _| {
            let mut c = Command::new("/usr/bin/open");
            c.args(["-n", "-a", "klogg"]).args(file);
            c
        }),
        quit_name: Some("klogg"),
        max_bytes: u64::MAX,
        control_open: false,
    });
    v.push(App {
        key: "sublime",
        name: "Sublime Text",
        installed: "/Applications/Sublime Text.app".into(),
        owner: "Sublime Text",
        processes: vec!["Sublime Text.app/".into()],
        launch: Box::new(|file, _| {
            let mut c = Command::new("/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl");
            c.arg("-n").args(file);
            c
        }),
        quit_name: Some("Sublime Text"),
        max_bytes: u64::MAX,
        control_open: false,
    });
    v.push(App {
        key: "vscode",
        name: "Visual Studio Code",
        installed: "/Applications/Visual Studio Code.app".into(),
        owner: "Code",
        processes: vec!["Visual Studio Code.app/".into()],
        launch: Box::new(|file, home| {
            let mut c = Command::new("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code");
            c.args(["--new-window", "--disable-extensions", "--skip-welcome", "--disable-workspace-trust"])
                .arg("--user-data-dir")
                .arg(home.join("vscode-user"))
                .arg("--extensions-dir")
                .arg(home.join("vscode-ext"))
                .args(file);
            c
        }),
        quit_name: Some("Visual Studio Code"),
        max_bytes: u64::MAX,
        control_open: false,
    });
    v
}

/// The JXA window watcher, started before the app.
struct Watcher(Child);

impl Watcher {
    fn start(script: &Path, owner: &str, needle: &str, limit_ms: u64) -> Result<Watcher, String> {
        let child = Command::new("osascript")
            .args(["-l", "JavaScript"])
            .arg(script)
            .args([owner, needle, &limit_ms.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("osascript: {e}"))?;
        // let it get past its own start-up before the app starts
        std::thread::sleep(Duration::from_millis(400));
        Ok(Watcher(child))
    }

    /// `Some((epoch ms, on screen))` once it saw the window, `Some((NaN, false))` when it gave up,
    /// `None` while it waits.
    fn poll(&mut self) -> Option<(f64, bool)> {
        match self.0.try_wait() {
            Ok(Some(_)) => {
                let mut s = String::new();
                if let Some(out) = self.0.stdout.as_mut() {
                    let _ = out.read_to_string(&mut s);
                }
                let mut w = s.split_whitespace();
                let at = w.next().and_then(|t| t.parse::<f64>().ok()).unwrap_or(f64::NAN);
                Some((at, w.next() == Some("on")))
            }
            Ok(None) => None,
            Err(_) => Some((f64::NAN, false)),
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn quit(app: &App, child: &mut Option<Child>) {
    if let Some(name) = app.quit_name {
        let _ = Command::new("osascript").args(["-e", &format!("tell application \"{name}\" to quit")]).output();
    } else {
        for pid in sample(&app.processes).2 {
            let _ = Command::new("kill").arg(&pid).status();
        }
    }
    let gone = Instant::now() + Duration::from_secs(15);
    while Instant::now() < gone && !sample(&app.processes).2.is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    for pid in sample(&app.processes).2 {
        let _ = Command::new("kill").args(["-9", &pid]).status();
    }
    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// One measured run.
#[derive(Default)]
struct Run {
    window_ms: Option<f64>,
    settled_ms: Option<f64>,
    rss_settle: Option<f64>,
    rss_peak: f64,
    idle_cpu: Option<f64>,
    /// The window was on screen when it was seen (a fullscreen app elsewhere keeps new windows
    /// on a Space nobody looks at: nothing is painted there).
    on_screen: bool,
}

/// Start `app` (with `file` or without), watch for its window and for it to settle.
fn run_once(app: &App, script: &Path, home: &Path, file: Option<&Path>, idle_secs: u64) -> Result<Run, String> {
    let needle = file.and_then(Path::file_name).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let limit = Duration::from_secs(300);
    let _ = std::fs::create_dir_all(home.join("keys"));
    let by_control = app.control_open && file.is_some();
    let mut watcher = Watcher::start(script, app.owner, if by_control { "" } else { &needle }, 300_000)?;
    let mut t0 = epoch_ms();
    let mut started = Instant::now();
    let mut child = (app.launch)(if by_control { None } else { file }, home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
        .map_err(|e| format!("{}: {e}", app.name))?;
    let _client = if let (true, Some(path)) = (by_control, file) {
        // the window first, then the open: only the open is timed
        let deadline = Instant::now() + Duration::from_secs(60);
        while watcher.poll().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_secs(1));
        let port: u16 = std::fs::read_to_string(home.join("port")).ok().and_then(|p| p.trim().parse().ok()).unwrap_or(0);
        let mut client = ashlar_control::Client::connect(port, &home.join("keys")).map_err(|e| format!("{}: control port: {e}", app.name))?;
        watcher = Watcher::start(script, app.owner, &needle, 300_000)?;
        t0 = epoch_ms();
        started = Instant::now();
        client
            .call("engine.execute", json!({"command": "log.open_dialog", "params": {"path": path.display().to_string()}}))
            .map_err(|e| format!("{}: log.open_dialog: {e}", app.name))?;
        Some(client)
    } else {
        None
    };
    let mut r = Run::default();
    let mut window: Vec<(Instant, f64)> = Vec::new();
    let result = loop {
        let now = Instant::now();
        if now.duration_since(started) > limit {
            break Err(format!("{} did not settle within {} s", app.name, limit.as_secs()));
        }
        if r.window_ms.is_none()
            && let Some((at, on)) = watcher.poll()
        {
            r.window_ms = Some(if at.is_nan() { f64::NAN } else { at - t0 });
            r.on_screen = on;
        }
        let (cpu, rss, pids) = sample(&app.processes);
        r.rss_peak = r.rss_peak.max(rss);
        if let Some(level) = memory_level().filter(|l| *l < LOW_MEMORY_LEVEL) {
            break Err(format!(
                "stopped after {:.1} s: the Mac's available memory fell to {level} % (app RSS {rss:.0} MB)",
                started.elapsed().as_secs_f64()
            ));
        }
        if rss > MAX_RSS_MB {
            break Err(format!("stopped after {:.1} s: RSS passed {:.0} GB", started.elapsed().as_secs_f64(), MAX_RSS_MB / 1024.0));
        }
        if !pids.is_empty() {
            window.push((now, cpu));
            window.retain(|(at, _)| now.duration_since(*at) <= Duration::from_millis(1000));
            if let Some((first_at, first_cpu)) = window.first().copied()
                && now.duration_since(first_at) >= Duration::from_millis(950)
                && cpu - first_cpu < 0.02
                && r.window_ms.is_some()
            {
                r.settled_ms = Some(ms(first_at.duration_since(started)));
                r.rss_settle = Some(rss);
                break Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if result.is_ok() && idle_secs > 0 {
        let (a, _, _) = sample(&app.processes);
        std::thread::sleep(Duration::from_secs(idle_secs));
        let (b, _, _) = sample(&app.processes);
        r.idle_cpu = Some(((b - a).max(0.0) / idle_secs as f64 * 100.0 * 1000.0).round() / 1000.0);
    }
    drop(watcher);
    quit(app, &mut child);
    result.map(|()| r)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn fresh(dir: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.to_path_buf())
}

fn measure_app(app: &App, script: &Path, dir: &Path, fixtures: &[Fixture], runs: usize, idle_secs: u64) -> Value {
    if !app.installed.exists() {
        return json!({"app": app.name, "key": app.key, "installed": false});
    }
    if !sample(&app.processes).2.is_empty() {
        return json!({"app": app.name, "key": app.key, "installed": true, "skipped": "already running (it would reuse that process); quit it and run again"});
    }
    let load = crate::bench::load();
    // launch to window, no file: one unmeasured launch first (first launch after install)
    let mut launch = Vec::new();
    let mut launch_on_screen = 0;
    let mut launch_error = None;
    for i in 0..=runs {
        eprintln!("bench: {} launch {i}/{runs}", app.name);
        let home = match fresh(&dir.join("home")) {
            Ok(h) => h,
            Err(e) => return json!({"app": app.name, "error": e}),
        };
        match run_once(app, script, &home, None, 0) {
            Ok(r) if i > 0 => {
                launch_on_screen += usize::from(r.on_screen);
                launch.extend(r.window_ms);
            }
            Ok(_) => {}
            Err(e) => {
                launch_error = Some(e);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let mut files = Vec::new();
    for f in fixtures {
        let name = format!("{} {}", f.format, f.label);
        if f.bytes > app.max_bytes {
            files.push(json!({"file": name, "skipped": format!("over {} MB, the largest size tried in {}", app.max_bytes >> 20, app.name)}));
            continue;
        }
        let (mut win, mut settled, mut rss, mut peak, mut idle) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), None);
        let mut error = None;
        let mut on_screen = 0;
        for i in 0..runs {
            eprintln!("bench: {} {name}: run {}/{runs}", app.name, i + 1);
            let home = match fresh(&dir.join("home")) {
                Ok(h) => h,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            };
            match run_once(app, script, &home, Some(&f.path), if i == 0 { idle_secs } else { 0 }) {
                Ok(r) => {
                    on_screen += usize::from(r.on_screen);
                    win.extend(r.window_ms);
                    settled.extend(r.settled_ms);
                    rss.extend(r.rss_settle);
                    peak.push(r.rss_peak);
                    if r.idle_cpu.is_some() {
                        idle = r.idle_cpu;
                    }
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let failed = error.is_some();
        files.push(json!({
            "file": name,
            "window_with_file_ms": summary(&win),
            "settled_ms": summary(&settled),
            "rss_after_open_mb": summary(&rss),
            "rss_peak_mb": summary(&peak),
            "idle_cpu_percent": idle,
            "idle_secs": idle_secs,
            "runs_with_window_on_screen": on_screen,
            "error": error,
        }));
        // a bigger file would only fail the same way, or worse
        if failed {
            break;
        }
    }
    json!({
        "app": app.name,
        "key": app.key,
        "installed": true,
        "version": version(app),
        "launch_to_window_ms": summary(&launch),
        "launch_runs_with_window_on_screen": launch_on_screen,
        "launch_error": launch_error,
        "files": files,
        "load_before": load,
    })
}

fn version(app: &App) -> Option<String> {
    let plist = app.installed.join("Contents/Info.plist");
    plist.exists().then(|| output("defaults", &["read", &plist.display().to_string(), "CFBundleShortVersionString"])).flatten()
}

/// `lnav -n -c ';SELECT …'` under `/usr/bin/time -l`: (wall ms, max RSS MB, the number printed).
fn lnav_sql(lnav: &Path, home: &Path, file: &Path, sql: &str) -> Result<(f64, Option<f64>, Option<u64>), String> {
    let t = Instant::now();
    let out = Command::new("/usr/bin/time")
        .arg("-l")
        .arg(lnav)
        .args(["-n", "-c", &format!(";{sql}")])
        .arg(file)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("lnav: {e}"))?;
    let wall = ms(t.elapsed());
    if !out.status.success() {
        return Err(format!("lnav -n failed ({})", out.status));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let rss = err
        .lines()
        .find(|l| l.contains("maximum resident set size"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse::<f64>().ok())
        .map(|b| b / f64::from(1u32 << 20));
    let n = String::from_utf8_lossy(&out.stdout).lines().rev().find_map(|l| l.trim().parse::<u64>().ok());
    Ok((wall, rss, n))
}

fn lnav(dir: &Path, fixtures: &[Fixture], runs: usize) -> Value {
    let Some(lnav) = ["/opt/homebrew/bin/lnav", "/usr/local/bin/lnav"].iter().map(PathBuf::from).find(|p| p.is_file()) else {
        return json!({"app": "lnav", "key": "lnav", "installed": false});
    };
    let ver = output(&lnav.display().to_string(), &["-V"]);
    let mut files = Vec::new();
    for f in fixtures {
        let name = format!("{} {}", f.format, f.label);
        // headless lnav reads, indexes and parses the whole file per query
        let runs = if f.bytes > 1 << 30 { runs.min(3) } else { runs };
        let mut error = None;
        eprintln!("bench: lnav {name}: {runs} runs per query");
        let field = if f.format == "jsonl" { r#"regexp('"status":5[0-9][0-9]', log_raw_text)"# } else { "log_level = 'error'" };
        let queries = [
            ("index", "SELECT count(*) FROM all_logs".to_string()),
            ("literal", "SELECT count(*) FROM all_logs WHERE log_raw_text LIKE '%timeout%'".to_string()),
            ("regex", "SELECT count(*) FROM all_logs WHERE regexp('r-[0-9a-f]{6}ff', log_raw_text)".to_string()),
            ("field", format!("SELECT count(*) FROM all_logs WHERE {field}")),
        ];
        let mut headless = serde_json::Map::new();
        let home = match fresh(&dir.join("home")) {
            Ok(h) => h,
            Err(e) => return json!({"app": "lnav", "error": e}),
        };
        {
            for (k, sql) in &queries {
                let (mut wall, mut mrss, mut count) = (Vec::new(), Vec::new(), None);
                for _ in 0..runs {
                    match lnav_sql(&lnav, &home, &f.path, sql) {
                        Ok((w, r, n)) => {
                            wall.push(w);
                            mrss.extend(r);
                            count = n;
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
                headless.insert((*k).to_string(), json!({"sql": sql, "wall_ms": summary(&wall), "max_rss_mb": summary(&mrss), "count": count}));
            }
        }
        let failed = error.is_some();
        files.push(json!({
            "file": name,
            "headless": headless,
            "tui": "not measured: lnav's terminal UI asks the terminal questions (cursor position, palette) and waits for answers a scripted pseudo-terminal does not give",
            "error": error,
        }));
        if failed {
            break;
        }
    }
    json!({"app": "lnav", "key": "lnav", "installed": true, "version": ver, "files": files})
}

/// The release binary in a minimal `Strata.app`, so LaunchServices starts it like an installed app.
fn strata_bundle(exe: &Path, dir: &Path) -> Result<PathBuf, String> {
    let app = dir.join("Strata.app");
    let macos = app.join("Contents").join("MacOS");
    let _ = std::fs::remove_dir_all(&app);
    std::fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
    std::fs::copy(exe, macos.join("strata")).map_err(|e| format!("copy strata: {e}"))?;
    let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>strata</string>
<key>CFBundleIdentifier</key><string>dev.strata.bench</string>
<key>CFBundleName</key><string>Strata</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#;
    std::fs::write(app.join("Contents").join("Info.plist"), plist).map_err(|e| e.to_string())?;
    Ok(app)
}

/// Measure the apps `o.apps` names (all by default), one after another, never two at once.
pub fn run(root: &Path, o: &Options, fixtures: &[Fixture]) -> Value {
    let dir = crate::bench::target_dir(root).join("bench").join("apps");
    let script = dir.join("window.js");
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&script, WINDOW_JS)) {
        return json!({"error": e.to_string()});
    }
    let runs = o.runs.min(5);
    let idle = o.idle_secs.min(30);
    let wanted = |k: &str| o.apps.as_ref().is_none_or(|a| a.iter().any(|x| x == k));
    let exe = crate::bench::target_dir(root).join("release").join("strata");
    let bundle = if exe.is_file() { strata_bundle(&exe, &dir).map_err(|e| eprintln!("bench: {e}")).ok() } else { None };
    let mut out = Vec::new();
    for app in apps(bundle) {
        if wanted(app.key) {
            out.push(measure_app(&app, &script, &dir, fixtures, runs, idle));
        }
    }
    if wanted("lnav") {
        out.push(lnav(&dir, fixtures, runs));
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_times_read_as_seconds() {
        assert_eq!(ps_time("0:01.50"), Some(1.5));
        assert_eq!(ps_time("1:02:03.00"), Some(3723.0));
        assert_eq!(ps_time("x"), None);
    }

    #[test]
    fn every_app_has_a_key_and_an_owner() {
        let a = apps(Some(PathBuf::from("/x/Strata.app")));
        assert_eq!(a.iter().map(|a| a.key).collect::<Vec<_>>(), ["strata", "console", "klogg", "sublime", "vscode"]);
        assert!(a.iter().all(|a| !a.owner.is_empty() && !a.processes.is_empty()));
    }
}
