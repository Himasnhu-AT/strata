# Benchmarks

How Strata compares with other log viewers on the same files, how every number was taken, and how
to reproduce it. Harness: `cargo xtask bench` (`xtask/src/bench*.rs`). Raw results:
`benches/results/2026-11-12-*.json` (`2026-11-12-merged.json` is what the tables show).

**Status (2026-11-12):** Strata's own rows are **pending re-measure**: the perf stream is rewriting
indexing and memory, and Strata will be measured again on that code (in-app and the outside
method, plain and the other formats, 1 GB and 5 GB). Preliminary Strata numbers from the old code
are in the raw JSON and are not quoted here. The competitor rows below stand.

Machine: Apple M5 Pro (Mac17,9), 15 cores, 24 GB, macOS 26.6.2 (25G83). The Mac was shared with
other agents' builds and in use by a person (load is recorded in each JSON). Medians of 5 runs per
app (9 for the CLI tools on files up to 1 GB, 1 for grep on 5 GB files).

## Plain-text `.log` (Strata's default format)

The `plain` fixture (`cargo xtask fixtures --format plain`, seed 42) is what Strata detects as
"Timestamped text": `[2026-11-12 00:00:00.035] INFO  search: order created (request r-…, 46 ms)`,
with Python tracebacks after some errors.

| File | App | File shown | Fully loaded | RSS after open | Peak RSS |
|---|---|---|---|---|---|
| plain 100M | **Strata** | pending re-measure | | | |
| | klogg | 438 ms | 1.11 s | 215 MB | 215 MB |
| | Sublime Text | 592 ms | 2.42 s | 594 MB | 594 MB |
| | VS Code (no screenshot) | 1.12 s | 7.92 s | 2.3 GB | 2.4 GB |
| | Console.app | 261 ms | 1.22 s | 783 MB | 783 MB |
| | lnav (headless) | — | 834 ms (load + index) | — | 114 MB (max RSS) |
| | `wc -l` (floor: read every byte) | — | 71.0 ms | — | 2 MB |
| | `less` (no index) | 4.5 ms | — (end after G: 4.8 ms) | — | — |
| plain 1G | **Strata** | pending re-measure | | | |
| | klogg | 421 ms | 1.26 s | 301 MB | 302 MB |
| | Sublime Text | 572 ms | 20.6 s | 3.8 GB | 3.8 GB |
| | VS Code (no screenshot) | 1.11 s | 8.67 s | 3.3 GB | 3.4 GB |
| | Console.app | stopped after 6.1 s: RSS passed 8 GB | | | |
| | lnav (headless) | — | 8.59 s (load + index) | — | 860 MB (max RSS) |
| | `wc -l` (floor: read every byte) | — | 659 ms | — | 2 MB |
| | `less` (no index) | 4.9 ms | — (end after G: 5.1 ms) | — | — |
| plain 5G | **Strata** | pending re-measure | | | |
| | klogg | 434 ms | 2.51 s | 382 MB | 382 MB |
| | Sublime Text | stopped after 80.3 s: RSS passed 8 GB | | | |
| | VS Code (no screenshot) | 1.14 s | 18.7 s | 6.1 GB | 6.6 GB |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| | lnav (headless) | — | 45.8 s (load + index) | — | 4.1 GB (max RSS) |
| | `wc -l` (floor: read every byte) | — | 3.76 s | — | 2 MB |
| | `less` (no index) | 5.0 ms | — (end after G: 5.3 ms) | — | — |

"File shown" for the GUI apps: from launching the app with the file to an on-screen window titled
with the file name. "Fully loaded": until the app's processes used under 20 ms of CPU in a whole
second (an upper bound of "indexed and usable"). See Method.

### Search, plain text (every matching line; counts in parentheses)

| File | Search | Query | Strata | Strata count | grep | lnav (headless, includes loading) |
|---|---|---|---|---|---|---|
| plain 100M | literal | `timeout` | pending re-measure | 6151 | 1.30 s (6151) | 934 ms (3308) |
| plain 100M | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 4870 | 543 ms (4870) | 991 ms (4870) |
| plain 100M | field | `level = error` | pending re-measure | 7034 | 601 ms (7034) | 808 ms (7034) |
| plain 1G | literal | `timeout` | pending re-measure | 64288 | 12.5 s (64288) | 9.70 s (34583) |
| plain 1G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 50518 | 5.30 s (50518) | 12.1 s (50518) |
| plain 1G | field | `level = error` | pending re-measure | 74073 | 5.30 s (74073) | 14.4 s (74073) |
| plain 5G | literal | `timeout` | pending re-measure | 321778 | 62.1 s (321778) | 47.3 s (173074) |
| plain 5G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 252693 | 26.4 s (252693) | 48.5 s (252693) |
| plain 5G | field | `level = error` | pending re-measure | 370509 | 26.4 s (370509) | 38.5 s (370509) |

lnav counts messages, not lines: it folds traceback lines into the message above them, so its
literal count is lower. The GUI apps' find boxes were not driven (synthetic keystrokes would
measure the automation, not the app).

### Launch to window (no file)

| App | Launch → window (no file) |
|---|---|
| **Strata** | pending re-measure |
| klogg | 378 ms |
| Sublime Text | 480 ms |
| VS Code | 469 ms |
| Console.app | 243 ms (overlapped another job's CI) |

## Other formats

JSON lines (100 MB, 1 GB, 5 GB) and, at 1 GB, logfmt, syslog (RFC 5424), Apache access logs and a
real macOS log (`/var/log/install.log` of this Mac, about 25 MB, repeated to 1 GB; not
reproducible byte for byte on another Mac). Rows a run did not reach are "not measured": the runs
were stopped by the owner before every app had covered every format.

| File | App | File shown | Fully loaded | RSS after open | Peak RSS |
|---|---|---|---|---|---|
| jsonl 100M | **Strata** | pending re-measure | | | |
| | klogg | 451 ms | 1.24 s | 210 MB | 210 MB |
| | Sublime Text | not measured | | | |
| | VS Code (no screenshot) | 1.12 s | 8.96 s | 2.3 GB | 2.4 GB |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| | lnav (headless) | — | 470 ms (load + index) | — | 251 MB (max RSS) |
| | `wc -l` (floor: read every byte) | — | 73.1 ms | — | 2 MB |
| | `less` (no index) | 4.7 ms | — (end after G: 5.0 ms) | — | — |
| jsonl 1G | **Strata** | pending re-measure | | | |
| | klogg | 451 ms | 1.66 s | 303 MB | 303 MB |
| | Sublime Text | not measured | | | |
| | VS Code (no screenshot) | 1.13 s | 7.93 s | 3.2 GB | 3.6 GB |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| | lnav (headless) | — | 5.44 s (load + index) | — | 2.2 GB (max RSS) |
| | `wc -l` (floor: read every byte) | — | 683 ms | — | 2 MB |
| | `less` (no index) | 5.0 ms | — (end after G: 5.2 ms) | — | — |
| jsonl 5G | **Strata** | pending re-measure | | | |
| | klogg | 440 ms | 1.99 s | 588 MB | 589 MB |
| | Sublime Text | not measured | | | |
| | VS Code (no screenshot) | 1.16 s | 14.0 s | 5.9 GB | 6.8 GB |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| | `wc -l` (floor: read every byte) | — | 3.22 s | — | 2 MB |
| | `less` (no index) | 5.1 ms | — (end after G: 5.3 ms) | — | — |
| logfmt 1G | **Strata** | pending re-measure | | | |
| | klogg | 426 ms | 1.66 s | 333 MB | 341 MB |
| | Sublime Text | not measured | | | |
| | VS Code | not measured | | | |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| syslog 1G | **Strata** | pending re-measure | | | |
| | klogg | 454 ms | 1.33 s | 296 MB | 296 MB |
| | Sublime Text | not measured | | | |
| | VS Code | not measured | | | |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| access 1G | **Strata** | pending re-measure | | | |
| | klogg | 464 ms | 1.29 s | 275 MB | 275 MB |
| | Sublime Text | not measured | | | |
| | VS Code | not measured | | | |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |
| macos 1G | **Strata** | pending re-measure | | | |
| | klogg | 454 ms | 1.31 s | 299 MB | 299 MB |
| | Sublime Text | not measured | | | |
| | VS Code | not measured | | | |
| | Console.app | not tried (the 1 GB file already passed 8 GB RSS) | | | |

### Search, other formats

| File | Search | Query | Strata | Strata count | grep | lnav (headless, includes loading) |
|---|---|---|---|---|---|---|
| jsonl 100M | literal | `timeout` | pending re-measure | 651 | 1.35 s (651) | 555 ms (651) |
| jsonl 100M | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 2254 | 574 ms (2254) | 575 ms (2254) |
| jsonl 100M | field | `status >= 500` | pending re-measure | 3109 | 786 ms (3109) | 706 ms (3109) |
| jsonl 1G | literal | `timeout` | pending re-measure | 6594 | 13.4 s (6594) | 6.06 s (6594) |
| jsonl 1G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 22988 | 5.52 s (22988) | 5.70 s (22988) |
| jsonl 1G | field | `status >= 500` | pending re-measure | 33083 | 6.77 s (33083) | 7.21 s (33083) |
| jsonl 5G | literal | `timeout` | pending re-measure | 33823 | 65.7 s (33823) | — |
| jsonl 5G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 114782 | 27.3 s (114782) | — |
| jsonl 5G | field | `status >= 500` | pending re-measure | 168962 | 32.8 s (168962) | — |
| logfmt 1G | literal | `timeout` | pending re-measure | 7979 | — | — |
| logfmt 1G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 27483 | — | — |
| logfmt 1G | field | `status >= 500` | pending re-measure | 39908 | — | — |
| syslog 1G | literal | `timeout` | pending re-measure | 13629 | — | — |
| syslog 1G | regex | `r-[0-9a-f]{6}ff` | pending re-measure | 46649 | — | — |
| syslog 1G | field | `level = error` | pending re-measure | 67979 | — | — |
| access 1G | literal | `/v1/pay` | pending re-measure | 1399757 | — | — |
| access 1G | regex | `" 50[0-9] ` | pending re-measure | 54973 | — | — |
| access 1G | field | `status >= 500` | pending re-measure | 54973 | — | — |
| macos 1G | literal | `error` | pending re-measure | 741330 | — | — |
| macos 1G | regex | `fail(ed|ure)` | pending re-measure | 150116 | — | — |
| macos 1G | field | `message ~ error` | pending re-measure | 741330 | — | — |

## Proof that the apps showed the file

At "fully loaded" each run brought the app to the front and captured its window
(`screencapture -x -o -l <window id>`) with its title and size, under `caffeinate -dimsu`, with
the screen-lock state checked. Shots are in `target/bench/shots/` of the bench worktree (not
committed, about 1 GB). Reviewed by eye: every shot looked at (klogg 5 GB plain and macOS, Sublime
1 GB, Console 100 MB, VS Code 100 MB and 5 GB) shows log text, no prompt or placeholder. Some
captures failed (the window id was gone when captured); rows keep their numbers.

- klogg, plain 100M: 5/5 shots, e.g. `target/bench/shots/plain-100M-klogg-1.png` — title “plain-104857600-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, plain 1G: 5/5 shots, e.g. `target/bench/shots/plain-1G-klogg-1.png` — title “plain-1073741824-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, plain 5G: 5/5 shots, e.g. `target/bench/shots/plain-5G-klogg-1.png` — title “plain-5368709120-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, jsonl 100M: 3/5 shots, e.g. `target/bench/shots/jsonl-100M-klogg-1.png` — title “jsonl-104857600-s42.jsonl - klogg (build 22.06.0.1289)”, 1372×777
- klogg, jsonl 1G: 4/5 shots, e.g. `target/bench/shots/jsonl-1G-klogg-1.png` — title “jsonl-1073741824-s42.jsonl - klogg (build 22.06.0.1289)”, 1372×777
- klogg, jsonl 5G: 5/5 shots, e.g. `target/bench/shots/jsonl-5G-klogg-1.png` — title “jsonl-5368709120-s42.jsonl - klogg (build 22.06.0.1289)”, 1372×777
- klogg, logfmt 1G: 5/5 shots, e.g. `target/bench/shots/logfmt-1G-klogg-1.png` — title “logfmt-1073741824-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, syslog 1G: 2/5 shots, e.g. `target/bench/shots/syslog-1G-klogg-3.png` — title “syslog-1073741824-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, access 1G: 4/5 shots, e.g. `target/bench/shots/access-1G-klogg-2.png` — title “access-1073741824-s42.log - klogg (build 22.06.0.1289)”, 1372×777
- klogg, macos 1G: 4/5 shots, e.g. `target/bench/shots/macos-1G-klogg-2.png` — title “macos-install-1073741824.log - klogg (build 22.06.0.1289)”, 1372×777
- Sublime Text, plain 100M: 5/5 shots, e.g. `target/bench/shots/plain-100M-sublime-1.png` — title “plain-104857600-s42.log”, 800×633
- Sublime Text, plain 1G: 3/5 shots, e.g. `target/bench/shots/plain-1G-sublime-1.png` — title “plain-1073741824-s42.log”, 800×633
- Console.app, plain 100M: 4/4 shots, e.g. `target/bench/shots/plain-100M-console-2.png` — title “plain-104857600-s42.log”, 650×500

Rows marked "(no screenshot)" come from an earlier run without captures. For VS Code that run was
also made **before** its large-file setting was set (below), so at 1 GB and 5 GB it may have shown
a confirmation instead of the text: treat those VS Code rows as unverified. The screenshot-checked
VS Code re-run (with the setting; its shots at 100 MB and 5 GB show log text) was stopped before
it saved results.

## Where Strata stands against these (from the old code, to be re-measured)

On the old code, Strata drew its first rows in about 50 ms at any size, faster than every GUI app
here (klogg about 430 ms, Sublime about 580 ms, VS Code about 1.1 s, all including their launch).
It lost to klogg on time to fully indexed (klogg 2.0–2.5 s for 5 GB, Strata about 11 s) and on
memory at 5 GB, and missed its own PLAN §5 budgets for index throughput (about 0.5 GB/s against
1 GB/s, limited by the per-chunk summaries), scroll CPU (p95 about 2 ms against 1 ms) and memory
(about 230 MB footprint at 100 MB against "100 MB plus the index"). The re-measure on the perf
stream's code will replace these.

## Running them

```sh
memgate -- cargo xtask bench --quick                      # 100 MB plain + jsonl, 3 runs (a few minutes)
memgate --profile release -- cargo run --release -p xtask -- bench   # the full matrix
cargo xtask bench --only strata --files plain:1G,plain:5G # one part (--only strata|others|apps)
cargo xtask bench --only apps --apps klogg --files plain:1G
cargo xtask bench --report a.json b.json --out merged.json  # merge parts, print the tables
```

The harness passes `--format` to `cargo xtask fixtures` for every file (seed 42), so the
generator's default format does not matter. It opens real windows and other apps: run it on a Mac
nobody is using, one tool at a time, under `caffeinate -dimsu`. CI never runs it.

## Method

Every Strata measurement starts a new process of the release build with an empty `HOME` (settings,
recent files and the autosaved workspace stay inside it), drives it over ashlar's control port and
quits it with `app.quit`. Each figure is the median of N runs (9 in the full run, 3 in `--quick`);
the JSON keeps every sample, min and max. Files are in the page cache (they were just generated or
read): cold-disk numbers would need `purge`, which needs root.

| Measure | How |
|---|---|
| Launch → first frame, warm | `launch_to_present_ms` the app writes to `STRATA_READY_FILE`: from the first line of `run()` to the first presented frame. Also spawn → ready file (exec, dyld, `main` included). One unmeasured launch first. Empty window. |
| Launch, cold-ish | Same, but the binary is copied to a new path and `HOME` is empty each run: code-signature checks, dyld and Metal shader caches miss, as on the first launch after installing. The disk cache is warm. At most 5 runs. |
| First lines on screen | `log.open_dialog {path}` sent → reply to `ui.wait {for: element, id: strata.table.row.0}`. The wait is answered on the frame that laid out the first row, so the figure includes the command round trip (well under 1 ms on loopback) and at most one frame. |
| Fully indexed | Same start → `log.files` reports the file `complete` (polled every 2 ms). |
| Index job | The app's own seconds for its `strata.open` + `strata.index` jobs (`app.stats` → `jobs.recent`): the line index plus the per-chunk time and level summaries. GB/s = file bytes / that time. |
| strata-core alone | In the xtask process, no window: `LogFile::open` + `index_all` (line index only), then `summarize_chunk` over every chunk, one thread, as the app's index job does. |
| Memory | After indexing and a 1 s settle: RSS (`ps`) and macOS physical footprint (`app.stats` → `process.footprint_bytes`). After the three searches: the OS's lifetime peak footprint. RSS counts the mapped file pages that are resident; footprint does not. |
| Search | `search.run` (literal `timeout`, case-insensitive; regex `r-[0-9a-f]{6}ff`) and `view.filter` (`status >= 500` on JSON lines, `level = error` on plain text) over the fully indexed file: the app's seconds for its `strata.search` / `strata.filter` job, which finds every match. The count is the toolbar's match counter (lines) or the status bar's "shown" lines. |
| Scroll CPU per frame | On the 1 GB JSON lines file (100 MB in `--quick`), indexed: `app.stats {reset}`, then `ui.scroll` on the table, one wheel event per request, reversing halfway: 480 steps of 36 pt ("wheel": rows nearby, mostly cached) and 120 steps of 20,000 pt ("jump": every frame parses rows it has not seen). `frame_cpu` is eframe's whole frame on the UI thread, painting included; "view code" is the runtime's `ui` part. |
| Idle | `app.stats` before and after 60 s (10 s in `--quick`) with an empty window, and while following the 1 GB file that does not change: frames drawn (the second `app.stats` wakes one frame, which is subtracted), wake reasons, process CPU time of every thread. |
| Binary size | `target/release/strata` as built, the same after `strip`, and ashlar's `release-small` profile (opt-level s, fat LTO, one codegen unit, stripped; passed with `--config` so the workspace profiles stay as they are). |

### Other tools

Nothing is installed for the benchmark: an app that is not on the Mac is reported as "not
installed here", with the command the harness would run.

| Tool | How |
|---|---|
| `wc -l` | The floor for "read every byte and count lines": wall time of the process, after one unmeasured run. |
| `grep` (macOS BSD grep 2.6) | `grep -c -i -F timeout`, `grep -c -E 'r-[0-9a-f]{6}ff'`, and the text equivalent of the field filter (an ERE per format, see `queries` in `bench.rs`): wall time and `/usr/bin/time -l` maximum RSS. Default locale. |
| `rg` | The same three searches when `rg` is on `PATH`. |
| `less` | In a pseudo-terminal (`script -q /dev/null less FILE`): ms until the file's first line is written to the terminal, then `G` and ms until the last line is. `less` keeps no index: it is the floor for "show the start" and "jump to the end". |
| Console.app (`open -n -a Console FILE`), VS Code (`code --new-window --disable-extensions FILE`, fresh profile), Sublime Text (`subl --safe-mode -n FILE`), klogg (`open -n -a klogg --args -n FILE`) | Launched with the file; a JXA script watches the window server (`CGWindowListCopyWindowInfo`) for the app's window titled with the file; `ps` sampled every 50 ms; "settled" = the first whole second in which the app's processes used under 20 ms of CPU. That is an upper bound of "fully open", not "first lines on screen", which cannot be seen from outside the app. RSS summed over the app's processes at that moment. Skipped when the app is already running; stopped (and reported) when the Mac's available memory falls under 12 % (`kern.memorystatus_level`) or the app's RSS passes 8 GB. At most 5 runs. |

### Covered windows

ashlar runs no UI pass for a window that is covered or minimized, except while a debugger request
(`ui.wait`, input, a screenshot) is pending. A search started by a command in a covered window
waits for the next pass. The harness therefore lets Strata's window come to the front (no
`STRATA_QUIET`) and follows every command with `ui.wait {for: idle}`, which keeps passes coming
if the window gets covered anyway. Frame CPU from a covered window is not comparable (nothing is
painted): the scroll rows report "window covered" when no frame was painted.

### App settings

Each app starts clean, the way a new user would see it:
- **VS Code:** a fresh profile per run (`--user-data-dir`, extensions off) with
  `{"workbench.editorLargeFileConfirmation": 1000000, "telemetry.telemetryLevel": "off", "update.mode": "none"}`
  in its `settings.json`. VS Code asks before opening files over about 1 GB; this setting opens them
  the way a user who clicks "Open anyway" does. VS Code then turns off tokenization, wrapping and
  folding for large files and says so in a notice (visible in the shots).
- **Sublime Text:** `--safe-mode`, a clean sandbox with default settings and no restored session.
- **klogg:** `-n` (a new session). Without it, klogg reopens every file of its last session, and a
  file handed over by `open` as an Apple event does not count as "a file was passed".
- **Console.app:** as it comes.
- **lnav:** headless only (`lnav -n -c ';SELECT …'`). Its terminal UI asks the terminal questions
  (cursor position, palette) and waits for answers a scripted pseudo-terminal does not give.
  lnav exited with status 1 on the 5 GB JSON lines file (not investigated).

### Caveats

- Console.app: its plain 100 MB run 1 was dropped (another stream's `cargo xtask ci` overlapped it),
  and its launch figure was taken in the same overlap.
- Several runs ran while other agents' builds shared the machine; memgate killed and retried
  some. Load before each part is in its JSON (`load_before`).
- "File shown" for the GUI apps includes their launch, since each was started with the file.
  Strata cannot be started with a log path (`strata FILE` opens an empty window), so the outside
  method opens the file over its control port after launch. Add Strata's launch to compare.
- No cold-disk numbers: files were in the page cache (`purge` needs root).
- ripgrep is not installed on this Mac.
