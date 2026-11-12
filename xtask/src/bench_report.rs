//! The results JSON of `cargo xtask bench` as markdown tables (medians; min–max where runs vary).

use std::fmt::Write as _;

use serde_json::Value;

/// `12.3` from a summary's median, `—` when absent.
fn med(v: &Value) -> Option<f64> {
    v["median"].as_f64()
}

fn ms(v: &Value) -> String {
    match med(v) {
        Some(m) if m >= 10_000.0 => format!("{:.1} s", m / 1000.0),
        Some(m) if m >= 100.0 => format!("{m:.0} ms"),
        Some(m) if m >= 10.0 => format!("{m:.1} ms"),
        Some(m) => format!("{m:.2} ms"),
        None => "—".into(),
    }
}

fn mb(v: &Value) -> String {
    med(v).map_or("—".into(), |m| format!("{m:.0} MB"))
}

fn num(v: &Value, digits: usize) -> String {
    v.as_f64().map_or("—".into(), |m| format!("{m:.digits$}"))
}

fn bytes_mb(v: &Value) -> String {
    v.as_f64().map_or("—".into(), |b| format!("{:.1} MB", b / 1_048_576.0))
}

/// The CLI result `key` for `file` (`wc_l`, `grep_literal`, …).
fn cli<'a>(r: &'a Value, file: &str, key: &str) -> &'a Value {
    r["others"]["cli"].as_array().and_then(|a| a.iter().find(|c| c["file"] == file)).map_or(&Value::Null, |c| &c["results"][key])
}

fn less<'a>(r: &'a Value, file: &str) -> &'a Value {
    r["others"]["less"].as_array().and_then(|a| a.iter().find(|c| c["file"] == file)).unwrap_or(&Value::Null)
}

fn gui_cell(app: &Value, file: &str, key: &str, fmt: fn(&Value) -> String) -> String {
    if app["installed"] == false {
        return "not installed here".into();
    }
    if let Some(s) = app["skipped"].as_str() {
        return format!("skipped ({s})");
    }
    let Some(f) = app["files"].as_array().and_then(|a| a.iter().find(|c| c["file"] == file)) else { return "—".into() };
    if let Some(s) = f["skipped"].as_str() {
        return format!("not tried ({s})");
    }
    if let Some(e) = f["error"].as_str() {
        return format!("failed: {e}");
    }
    fmt(&f[key])
}

/// The headline table: Strata (its own instruments, and the outside method) next to every other
/// app, file by file.
fn comparison(s: &mut String, r: &Value) {
    let apps: Vec<&Value> = r["apps"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    if apps.is_empty() {
        return;
    }
    let _ = writeln!(s, "\n## Comparison\n");
    let _ = writeln!(
        s,
        "Medians. \"Strata (in-app)\" uses Strata's own instruments; every other row, Strata's included, was taken from outside the app by the same method (see Method). — = not observable for that app.\n"
    );
    let _ = writeln!(s, "| App | Launch → window (no file) |\n|---|---|");
    let st = &r["strata"];
    if !st.is_null() {
        let _ = writeln!(s, "| Strata (in-app: launch → first frame presented) | {} |", ms(&st["launch"]["warm"]["launch_to_present_ms"]));
    }
    for a in &apps {
        let cell = if a["installed"] == false {
            "not installed here".to_string()
        } else if let Some(e) = a["launch_error"].as_str() {
            format!("failed: {e}")
        } else if a["key"] == "lnav" {
            "— (terminal app)".to_string()
        } else {
            ms(&a["launch_to_window_ms"])
        };
        let _ = writeln!(s, "| {} | {cell} |", a["app"].as_str().unwrap_or("?"));
    }
    let _ = writeln!(
        s,
        "\n| File | App | Open → file shown | Fully loaded | RSS after open | Peak RSS | Idle CPU after open | Literal search |\n|---|---|---|---|---|---|---|---|"
    );
    let mut names: Vec<String> = Vec::new();
    for f in st["files"].as_array().into_iter().flatten() {
        names.push(format!("{} {}", f["format"].as_str().unwrap_or("?"), f["size"].as_str().unwrap_or("?")));
    }
    for a in &apps {
        for f in a["files"].as_array().into_iter().flatten() {
            if let Some(n) = f["file"].as_str()
                && !names.iter().any(|x| x == n)
            {
                names.push(n.to_string());
            }
        }
    }
    for name in &names {
        if let Some(f) = st["files"]
            .as_array()
            .and_then(|fs| fs.iter().find(|f| format!("{} {}", f["format"].as_str().unwrap_or(""), f["size"].as_str().unwrap_or("")) == *name))
        {
            let idle = &st["idle"]["following"];
            let idle_cell = if st["idle"]["file"].as_str() == Some(name.as_str()) {
                format!("{} % (following)", num(&idle["cpu_percent"], 3))
            } else {
                "—".into()
            };
            let _ = writeln!(
                s,
                "| {name} | Strata (in-app) | {} (first rows drawn) | {} (indexed) | {} | {} (footprint) | {idle_cell} | {} |",
                ms(&f["open"]["first_lines_ms"]),
                ms(&f["open"]["indexed_ms"]),
                mb(&f["memory"]["rss_after_open_mb"]),
                mb(&f["memory"]["peak_footprint_after_searches_mb"]),
                ms(&f["search"]["literal"]["job_ms"]),
            );
        }
        for a in &apps {
            let app = a["app"].as_str().unwrap_or("?");
            if a["installed"] == false {
                let _ = writeln!(s, "| {name} | {app} | not installed here | | | | | |");
                continue;
            }
            if let Some(why) = a["skipped"].as_str() {
                let _ = writeln!(s, "| {name} | {app} | skipped: {why} | | | | | |");
                continue;
            }
            let Some(f) = a["files"].as_array().and_then(|fs| fs.iter().find(|f| f["file"] == name.as_str())) else {
                let _ = writeln!(s, "| {name} | {app} | not run | | | | | |");
                continue;
            };
            if let Some(why) = f["skipped"].as_str() {
                let _ = writeln!(s, "| {name} | {app} | not tried ({why}) | | | | | |");
                continue;
            }
            if let Some(e) = f["error"].as_str()
                && f["settled_ms"].is_null()
            {
                let _ = writeln!(s, "| {name} | {app} | failed: {e} | | | | | |");
                continue;
            }
            if a["key"] == "lnav" {
                let h = &f["headless"];
                let _ = writeln!(
                    s,
                    "| {name} | {app} | not measured (TUI) | {} (headless load + index) | — | {} (max RSS, headless) | not measured (TUI) | {} (headless, includes loading) |",
                    ms(&h["index"]["wall_ms"]),
                    mb(&h["index"]["max_rss_mb"]),
                    ms(&h["literal"]["wall_ms"]),
                );
                continue;
            }
            let shown = format!("{} (window titled with the file)", ms(&f["window_with_file_ms"]));
            let search = "not driven".to_string();
            let idle = f["idle_cpu_percent"].as_f64().map_or("—".into(), |v| format!("{v:.2} %"));
            let _ = writeln!(
                s,
                "| {name} | {app} | {shown} | {} (CPU quiet) | {} | {} | {idle} | {search} |",
                ms(&f["settled_ms"]),
                mb(&f["rss_after_open_mb"]),
                mb(&f["rss_peak_mb"]),
            );
        }
    }
}

pub fn markdown(r: &Value) -> String {
    let mut s = String::new();
    let m = &r["machine"];
    let _ = writeln!(
        s,
        "# Strata benchmarks ({} run, {} runs each, {})\n",
        r["mode"].as_str().unwrap_or("?"),
        r["runs"],
        r["date"].as_str().unwrap_or("?")
    );
    let _ = writeln!(
        s,
        "Machine: {} ({}), {} cores, {} GB, {}. Commit {}. Load before: {}.\n",
        m["cpu"].as_str().unwrap_or("?"),
        m["model"].as_str().unwrap_or("?"),
        m["cores"],
        num(&m["memory_gb"], 0),
        m["os"].as_str().unwrap_or("?"),
        r["git"].as_str().unwrap_or("?"),
        r["load_before"]["loadavg"].as_str().unwrap_or("?")
    );
    let st = &r["strata"];
    let files: Vec<&Value> = st["files"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let gui: Vec<&Value> = r["others"]["gui"].as_array().map(|a| a.iter().collect()).unwrap_or_default();

    comparison(&mut s, r);

    if !files.is_empty() {
        let _ = writeln!(s, "\n## Open\n");
        let mut head = "| File | Strata first lines | Strata indexed | index job (GB/s) | wc -l | less first screen | less G (end) |".to_string();
        let mut rule = "|---|---|---|---|---|---|---|".to_string();
        for g in &gui {
            let _ = write!(head, " {} settled |", g["app"].as_str().unwrap_or("?"));
            rule.push_str("---|");
        }
        let _ = writeln!(s, "{head}\n{rule}");
        for f in &files {
            let name = format!("{} {}", f["format"].as_str().unwrap_or("?"), f["size"].as_str().unwrap_or("?"));
            let o = &f["open"];
            let mut row = format!(
                "| {name} | {} | {} | {} ({}) | {} | {} | {} |",
                ms(&o["first_lines_ms"]),
                ms(&o["indexed_ms"]),
                ms(&o["index_job_ms"]),
                med(&o["index_job_gb_per_s"]).map_or("—".into(), |g| format!("{g:.2}")),
                ms(&cli(r, &name, "wc_l")["wall_ms"]),
                ms(&less(r, &name)["first_screen_ms"]),
                ms(&less(r, &name)["end_after_g_ms"]),
            );
            for g in &gui {
                let _ = write!(row, " {} |", gui_cell(g, &name, "settled_ms", ms));
            }
            let _ = writeln!(s, "{row}");
        }

        let _ = writeln!(s, "\n## Memory\n");
        let mut head = "| File | Strata RSS after open | footprint after open | peak footprint after searches | grep -c max RSS |".to_string();
        let mut rule = "|---|---|---|---|---|".to_string();
        for g in &gui {
            let _ = write!(head, " {} RSS |", g["app"].as_str().unwrap_or("?"));
            rule.push_str("---|");
        }
        let _ = writeln!(s, "{head}\n{rule}");
        for f in &files {
            let name = format!("{} {}", f["format"].as_str().unwrap_or("?"), f["size"].as_str().unwrap_or("?"));
            let mm = &f["memory"];
            let mut row = format!(
                "| {name} | {} | {} | {} | {} |",
                mb(&mm["rss_after_open_mb"]),
                mb(&mm["footprint_after_open_mb"]),
                mb(&mm["peak_footprint_after_searches_mb"]),
                mb(&cli(r, &name, "grep_literal")["max_rss_mb"]),
            );
            for g in &gui {
                let _ = write!(row, " {} |", gui_cell(g, &name, "rss_at_settle_mb", mb));
            }
            let _ = writeln!(s, "{row}");
        }

        let _ = writeln!(s, "\n## Search (all matching lines)\n");
        let _ = writeln!(
            s,
            "| File | Search | Query | Strata job | Strata MB/s | Strata count | grep -c | grep count | rg -c |\n|---|---|---|---|---|---|---|---|---|"
        );
        for f in &files {
            let name = format!("{} {}", f["format"].as_str().unwrap_or("?"), f["size"].as_str().unwrap_or("?"));
            for kind in ["literal", "regex", "field"] {
                let q = &f["search"][kind];
                let g = cli(r, &name, &format!("grep_{kind}"));
                let rg = cli(r, &name, &format!("rg_{kind}"));
                let rg_cell = if rg.is_null() {
                    if r["others"].is_null() { "—".into() } else { "not installed here".to_string() }
                } else {
                    ms(&rg["wall_ms"])
                };
                let _ = writeln!(
                    s,
                    "| {name} | {kind} | `{}` | {} | {} | {} | {} | {} | {rg_cell} |",
                    q["query"].as_str().unwrap_or("?"),
                    ms(&q["job_ms"]),
                    med(&q["mb_per_s"]).map_or("—".into(), |v| format!("{v:.0}")),
                    q["count"].as_u64().map_or("—".into(), |c| c.to_string()),
                    ms(&g["wall_ms"]),
                    g["output"].as_str().unwrap_or("—"),
                );
            }
        }
    }

    if let Some(core) = st["core"].as_array() {
        let _ =
            writeln!(s, "\n## strata-core alone (no window)\n\n| File | open + line index | GB/s | chunk summaries | GB/s |\n|---|---|---|---|---|");
        for c in core {
            if let Some(why) = c["skipped"].as_str() {
                let _ = writeln!(s, "| {} | {why} | | | |", c["file"].as_str().unwrap_or("?"));
                continue;
            }
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} |",
                c["file"].as_str().unwrap_or("?"),
                ms(&c["open_and_index_ms"]),
                med(&c["index_gb_per_s"]).map_or("—".into(), |v| format!("{v:.2}")),
                ms(&c["summaries_ms"]),
                med(&c["summaries_gb_per_s"]).map_or("—".into(), |v| format!("{v:.2}")),
            );
        }
    }

    if !st.is_null() {
        let l = &st["launch"];
        let _ = writeln!(s, "\n## Launch, scroll, idle, size\n\n| Measure | Strata |\n|---|---|");
        let _ = writeln!(s, "| Launch → first frame presented, warm (app's figure) | {} |", ms(&l["warm"]["launch_to_present_ms"]));
        let _ = writeln!(s, "| Spawn → ready file, warm | {} |", ms(&l["warm"]["spawn_to_ready_ms"]));
        let _ = writeln!(
            s,
            "| Launch → first frame presented, cold-ish (new binary path, empty HOME) | {} |",
            ms(&l["cold_ish"]["launch_to_present_ms"])
        );
        let _ = writeln!(s, "| Spawn → ready file, cold-ish | {} |", ms(&l["cold_ish"]["spawn_to_ready_ms"]));
        let _ = writeln!(
            s,
            "| Empty window after 2 s: RSS / footprint | {} / {} |",
            mb(&l["idle_empty_window"]["rss_mb"]),
            mb(&l["idle_empty_window"]["footprint_mb"])
        );
        let sc = &st["scroll"];
        for k in ["wheel", "jump"] {
            let v = &sc[k];
            if !v.is_null() {
                let cpu = if v["painted_frames"].as_u64() == Some(0) {
                    "window covered, nothing painted".to_string()
                } else {
                    format!("{} / {} ms", num(&v["frame_cpu_p50_ms"], 3), num(&v["frame_cpu_p95_ms"], 3))
                };
                let _ = writeln!(
                    s,
                    "| Scroll {k} ({}, {} steps of {} pt): frame CPU p50 / p95; view code p50 / p95 | {cpu}; {} / {} ms |",
                    sc["file"].as_str().unwrap_or("?"),
                    v["steps"],
                    v["dy_pt"],
                    num(&v["ui_p50_ms"], 3),
                    num(&v["ui_p95_ms"], 3)
                );
            }
        }
        for k in ["empty", "following"] {
            let v = &st["idle"][k];
            if !v.is_null() {
                let label = if k == "empty" {
                    "Idle, empty window".to_string()
                } else {
                    format!("Idle, following an unchanged file ({})", st["idle"]["file"].as_str().unwrap_or("?"))
                };
                // input frames: a person moved the pointer over the window, the app did not wake itself
                let input = v["frames_by_reason"]["input"].as_u64().unwrap_or(0);
                let own = v["frames_excluding_probe"].as_u64().unwrap_or(0).saturating_sub(input);
                let note = if input > 0 { format!(" (+{input} input frames from the pointer)") } else { String::new() };
                let _ = writeln!(
                    s,
                    "| {label}, {} s: frames the app woke for / CPU | {own} frames{note} / {} % |",
                    v["seconds"],
                    num(&v["cpu_percent"], 3)
                );
            }
        }
        let b = &st["binary"];
        let _ = writeln!(
            s,
            "| Binary: release / stripped / release-small | {} / {} / {} |",
            bytes_mb(&b["release_bytes"]),
            bytes_mb(&b["release_stripped_bytes"]),
            bytes_mb(&b["release_small_bytes"])
        );
    }

    if !gui.is_empty() {
        let _ = writeln!(s, "\n## Other apps\n");
        for g in &gui {
            let state = if g["installed"] == false {
                "not installed here".to_string()
            } else {
                g["skipped"].as_str().map_or("measured".into(), |x| format!("skipped: {x}"))
            };
            let _ = writeln!(s, "- {}: {state}", g["app"].as_str().unwrap_or("?"));
        }
        if r["others"]["versions"]["rg"].is_null() {
            let _ = writeln!(s, "- ripgrep: not installed here");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_and_missing_values_render() {
        assert_eq!(ms(&json!({"median": 12_345.0})), "12.3 s");
        assert_eq!(ms(&json!({"median": 150.4})), "150 ms");
        assert_eq!(ms(&json!({"median": 4.256})), "4.26 ms");
        assert_eq!(ms(&Value::Null), "—");
    }

    #[test]
    fn an_empty_result_still_renders() {
        let md = markdown(&json!({"mode": "quick", "runs": 3}));
        assert!(md.starts_with("# Strata benchmarks"));
    }

    #[test]
    fn apps_not_installed_say_so() {
        let g = json!({"app": "klogg", "installed": false});
        assert_eq!(gui_cell(&g, "jsonl 100M", "settled_ms", ms), "not installed here");
    }
}
