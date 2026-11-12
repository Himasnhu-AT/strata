# Strata plan

Strata is a desktop log viewer for very large files, built on ashlar. A person and an AI agent read
the same log in the same window: the agent drives Strata over MCP, and everything it does is
visible and undoable.

Development starts after ashlar 0.2 is on crates.io.

## 1. Who it is for

- **Backend and platform engineers** reading service logs pulled from a server or a CI run: often
  hundreds of MB to several GB, JSON lines or logfmt, and they need "what happened around 14:02".
- **People debugging with an AI agent.** Today the agent greps the file from a terminal and pastes
  fragments; the person can't see what it looked at. In Strata they share one view.
- **Desktop and mobile developers** reading crash logs, device logs and app logs (syslog, plain
  text with timestamps).

### The jobs

1. Open a huge file and see the first lines at once, whatever its size.
2. Find the first error, and the lines around it, fast.
3. Narrow to one service, request id or time window.
4. Follow a live file while reproducing a bug.
5. See where errors cluster in time.
6. Hand the investigation to an agent, and check what it found.

## 2. Features

### MVP (first public release)

| Feature | Notes |
|---|---|
| Open any size | Memory-mapped; a background job builds the line index in chunks. The first screen shows as soon as the first chunk is indexed; scrolling past the indexed part waits on that chunk only. |
| Follow (tail) | Watches the file; appends new lines, keeps the view pinned to the bottom unless the user scrolled away. Detects truncation and rotation (file replaced) and says so. |
| Search | Text and regex, case toggle, whole word. Runs as a job over the index with progress; results stream into a match list; next/previous with F3 / Shift+F3. |
| Filters | By level, by time range, by any parsed field (`service = api`, `status >= 500`), and "lines matching / not matching". Filters stack; each is a chip that can be turned off. |
| Formats | Auto-detected per file from the first 64 KB: JSON lines, logfmt, syslog (RFC 3164 and 5424), common timestamp-prefixed text (ISO 8601, `[2026-11-12 07:41:03.118]`, Apache/nginx access). Plain text is the fallback. |
| Columns | Parsed fields become columns: time, level and message always; others can be shown, hidden and reordered. |
| Detail pane | The selected line, pretty-printed (JSON as a tree), with copy as text or JSON. |
| Time histogram | A strip above the table: lines per time bucket, errors stacked in red. Drag to select a time range (becomes a filter). |
| Jump to error | Next / previous error or warning, from the keyboard. |
| Bookmarks | Named, colored, listed in the sidebar, saved per file. |
| Compressed files | `.gz` opens through a decompress-and-index job into a temp file (streamed, capped by free disk). |
| Recent files and saved filters | In the sidebar. |
| MCP and CLI | The full command table below, from day one. |
| Agent panel | What the agent ran, what it highlighted, one-step undo. |

### Later

- Merge several files into one timeline by timestamp (multi-service view).
- `.zst`, `.bz2`; logs inside `.zip` / `.tar.gz`.
- Diff two logs (a good run against a bad one), aligned by message template.
- Message templates ("these 40,000 lines are the same message with different ids"): group and
  count.
- Remote sources: `ssh host tail -f`, `kubectl logs -f`, `docker logs -f` as a followed stream.
- Custom format definitions (a regex with named groups), shared as files.
- Export the filtered lines, or a share bundle (lines + filters + bookmarks).

## 3. The AI and MCP surface

Strata's MCP server (ashlar's `<app>-cli mcp`, or the running window's control port with
`--connect`) exposes these commands. Read-only commands never change what the user sees.
Commands that do change the view are recorded in the agent panel and undoable.

`file` defaults to the active file in every command (so shortcuts and single-file sessions work
with `{}`); lines are 1-based; times are RFC 3339 (UTC when no zone). The frozen result shapes are
`crates/app/src/results.rs` and the golden files in `crates/app/tests/mcp/` (docs/contracts.md).

| Command | Params | Result | Changes the view? |
|---|---|---|---|
| `log.open` | `path`, `follow?` | file id, size, detected format, line count so far, indexing state | yes (opens a tab) |
| `log.files` | — | open files with id, path, format, lines, time span | no |
| `log.stats` | `file`, `range?` (`{"from"?: time, "to"?: time}`) | lines, bytes, time span, counts per level, top fields and values | no |
| `log.search` | `file`, `query`, `regex?`, `case?`, `whole_word?`, `limit?` (default 50, max 500), `filter?` | matches: line number, time, level, text (redacted, cut to 2 KB) and the total count | no |
| `log.lines` | `file`, `from`, `count` (max 500) | lines with parsed fields (redacted) | no |
| `log.around` | `file`, `line`, `before?`, `after?` (max 200 each) | the context lines | no |
| `log.histogram` | `file`, `bucket?` (`auto` or a duration: `30s`, `5m`), `filter?` | buckets with counts per level | no |
| `log.fields` | `file` | parsed field names, types and the 20 most common values each | no |
| `log.first` | `file`, `filter` | the first line matching a filter (e.g. first `level = error` after a time) | no |
| `view.filter` | `file`, `filter` (same grammar as the filter bar) | the visible line count | yes |
| `view.clear_filters` | `file` | — | yes |
| `view.goto` | `file`, `line` or `time` | — | yes (scrolls) |
| `view.highlight` | `file`, `lines` (array of line numbers) or `query`, `note?` | highlight id | yes |
| `bookmark.add` | `file`, `line?` (default: selected), `name?` (default `Line <n>`), `note?` | bookmark id | yes |
| `bookmark.list` | `file` | bookmarks | no |
| `agent.note` | `text` | — | yes (a note in the agent panel: the agent's summary for the person) |
| `agent.undo` | — | — | yes (undoes every agent change since the person last acted) |

Limits are part of the contract: every list result is capped and says how many it left out, so an
agent never pulls a 5 GB file into its context by accident.

### The agent panel

A 360 px panel on the right, opened when an agent connects (or from View ▸ Agent):

- A header with the client's name ("via MCP · Claude") and a connection dot.
- A timeline of what the agent did: each command as one row (icon, short description, line counts),
  newest last. Read-only calls are grey and collapsible ("looked at 3 searches"); calls that
  changed the view are bold and clickable (jumps to the line, shows the highlight).
- The agent's notes (`agent.note`) as text blocks.
- "Undo agent changes" removes every highlight, bookmark and filter the agent added since the
  person last acted (one undo step in ashlar's history).
- A switch "Allow the agent to change the view": off means `view.*` and `bookmark.add` return an
  error and only read-only commands work.

### Privacy

- Local only: no network access except the update check (ashlar-update), which can be turned off.
- Log text leaves the app only as the result of an MCP call. Results pass a redaction filter first:
  bearer tokens, `Authorization` headers, AWS-style keys, private keys, JWTs, emails and long hex
  or base64 secrets are replaced with `[redacted:<kind>]`. Patterns are listed in Settings and can
  be extended; redaction can be turned off per file, with a warning.
- MCP is opt-in: off until the person enables it in Settings ▸ AI (ashlar-agents installs the MCP
  config for Claude, Cursor and others).
- Telemetry (ashlar-trace) is off by default and never includes log contents or paths.

## 4. Architecture on ashlar

One app crate plus a headless core crate, so the core runs in tests, the CLI and MCP without a
window.

```
strata/
  crates/
    core/      strata-core: file index, formats, search, filters, histogram, redaction (no egui)
    app/       strata: the ashlar App, views, commands, MCP surface
  benches/     benchmark scripts and fixtures generator
```

| Need | ashlar piece |
|---|---|
| App shell, menus, palette, shortcuts, crash guard, autosave of state | `ashlar::app::App<D>` with commands (`CommandSpec::new` with the id constants and `.query()` for read-only ones; `cmd!` / `query!` need literal ids) |
| Document | `Session<StrataDoc>`: the document is the workspace (open files, filters, columns, bookmarks, highlights), not the log text. `s.edit(label, ...)` makes every change one undo step. Open `LogFile`s live beside it in `StrataDoc::live` (not saved). ashlar's built-in `file.*` commands open and save the workspace (`.strata`). |
| Background work | ashlar jobs (`ashlar-jobs`): `Progress::steps`, `CancelToken`, results applied on the UI thread with `apply` as one undo step. Indexing, search, histogram, gz decompress are jobs. Jobs are started from views (`cx.jobs`): a command running on `Session` cannot spawn one in 0.2. |
| The log table | `ashlar::ui::virtual_list::VirtualTable` (O(visible) per frame; `VirtualList::type_ahead`), `selection::Selection`. 0.2 caps a list at `virtual_list::MAX_ROWS` (10 M) rows and has no column reorder/hide: the ui stream windows bigger files |
| Detail pane | `ashlar::ui::code_view::CodeView` with `.lang(Some("json"))` (ashlar-syntax underneath, no direct dependency) |
| Sidebar | `ashlar::ui::sidebar` (sections for Files, Bookmarks, Saved filters) |
| Toolbar, chips, histogram | `ashlar::ui::controls`, `ashlar::ui::overlay::Select`, custom painter for the histogram |
| Files | `ashlar-fs`: recent files, settings, per-app folders; `App::accept_drops`, open from Finder |
| Native | `ashlar-native`: file dialogs, menu bar, notifications for "error appeared while following" |
| Agent control | the command registry exposed by `App::cli()` → `mcp`, and the control port; `ashlar-agents` (its own crate, not re-exported by `ashlar-rs`) for Settings ▸ AI |
| Testing | `--headless --snap/--tree`, `.ui` scripts, `Driver::assert_snapshot`, named scenes (`App::scene`) for every screen |
| Telemetry | the `ashlar-rs` `telemetry` feature (`App::telemetry`), off by default |

### Data model (strata-core)

- `LogFile` (shared as `Arc<LogFile>`, `&self` methods): id, path, map, format, `LineIndex`, field names; `Watcher` for follow
- `LineIndex`: line start offsets in chunks of 64 K lines (`Vec<u64>` per chunk; about 8 bytes a
  line, 400 MB for 50 M lines, so chunks past a budget are stored compressed as deltas, 2–3 bytes a
  line), plus per chunk: min/max timestamp and level counts, which make histogram, time filters and
  "first error after T" skip whole chunks.
- `Parsed { time: Option<Timestamp>, level: Option<Level>, fields: Vec<(FieldId, Span)>, message: Span }`
  (`Timestamp` = µs since the epoch, UTC)
  parsed lazily for visible lines and cached by line number (LRU).
- `Filter`: a small expression tree (field comparisons, text/regex, time range, not/and/or) with a
  text form shared by the filter bar, the CLI and MCP.
- `StrataDoc` (the ashlar document): open files (paths, not contents), filters, columns,
  bookmarks, highlights, agent changes. Serializable, so the workspace restores after a crash.

## 5. Performance budgets

Checked in CI with ashlar's bench gate and in the benchmark suite.

| Budget | Target |
|---|---|
| First lines on screen after open, any size | under 150 ms |
| Index throughput | at least 1 GB/s on an M-series Mac (memchr over the mmap) |
| Scroll CPU per frame (any file size) | under 1 ms |
| Memory | under 100 MB plus the line index (about 3 bytes per line compressed) |
| Regex search | at least 500 MB/s single literal; multi-threaded over chunks |
| Idle CPU (window open, not following) | 0 % |
| Idle CPU while following a file with no new lines | 0 % (file watcher, no polling) |
| Launch to first frame | under 200 ms |

## 6. Benchmarks (published)

Compared with: VS Code, Sublime Text, klogg, and macOS Console.app (where it can open the file).

| Measure | Files |
|---|---|
| Time to first lines on screen | 100 MB, 1 GB, 5 GB JSON lines and plain text |
| Time to fully open / index | same |
| Memory (RSS and footprint) after open, after a search | same |
| Regex search time, first match and all matches | 1 GB, 5 GB |
| Scroll CPU per frame (scripted 10 s scroll) | 1 GB |
| Idle CPU over 60 s; idle CPU while following a file that grows 100 lines/s | — |
| Launch to first frame, cold and warm | empty |
| Download size and installed size | — |

Method: the same Mac (model and macOS version stated), each measurement the median of 9 runs, files
generated by a published script with a fixed seed, apps at their default settings, every script and
raw result in `benches/`. Where another app wins a measure, the results say so.

## 7. Milestones

Week 0 is the week ashlar 0.2 is on crates.io.

| Week | Delivers |
|---|---|
| 1 | Contracts frozen (section 8). strata-core: mmap, chunked line index job, plain + JSON lines formats. App shell: open, virtual table, status bar. |
| 2 | Search (text, regex) as a job, filters and filter bar, level chips, detail pane, follow mode. |
| 3 | Histogram, bookmarks, sidebar, logfmt / syslog / timestamp formats, .gz. MCP commands complete with limits and redaction. |
| 4 | Agent panel and undo, Settings ▸ AI, palette polish, empty/error states, accessibility pass, golden snapshots for every screen. |
| 5 | Benchmarks and the write-up, packaging (signed macOS build, Linux, Windows), docs site page, launch video ("the agent finds the crash"). |

## 8. Parallel work

The owner builds Strata with several agents at once, each in its own git worktree. The plan is split
so streams rarely touch the same files.

### Frozen first (week 1, day 1–2, by the lead)

Nothing else starts until these are merged to `main`:

1. **Command contract:** every command id and its params from section 3, plus the user-only
   commands (`log.open_dialog`, `log.close`, `search.next`, `view.toggle_column`…; `file.*` ids
   are ashlar's workspace built-ins), in `crates/app/src/commands/ids.rs` as constants with `Param` lists and stub handlers that return
   `CommandError::failed(id, "not yet")`.
2. **Core types:** `strata-core` public types (`LogFile`, `LineIndex` API, `Parsed`, `Level`,
   `Filter` and its text grammar, `Match`, `Histogram`, `Redaction`) with signatures and doc
   comments, bodies `todo`-free stubs returning empty results.
3. **MCP result schemas:** the JSON shape of each result in section 3, with one golden JSON file
   per command under `crates/app/tests/mcp/`.
4. **Document type:** `StrataDoc` fields and the undo labels.

Changes to these after the freeze are additive only, announced to every stream that uses them and
to the lead.

### Workstreams

| Stream | Owns (files) | Depends on | Done when |
|---|---|---|---|
| `index` | `crates/core/src/{mmap,index,follow,gz}.rs` | core types | 5 GB file: first chunk under 150 ms, ≥1 GB/s, follow handles append, truncate, rotate; fuzz test on random bytes |
| `formats` | `crates/core/src/formats/*`, `crates/core/src/redact.rs` | core types | detection and parsing for plain, JSON lines, logfmt, syslog, timestamp text; property tests; redaction patterns with tests |
| `search` | `crates/core/src/{search,filter,histogram}.rs` | core types, `index` API (stubs suffice) | text/regex search ≥500 MB/s, filter grammar round-trips, histogram per chunk; benches in the gate |
| `ui` | `crates/app/src/views/*` (table, toolbar, histogram strip, detail pane, sidebar, status bar, empty/error states) | command contract, core types | every screen in docs/DESIGN.md as a named scene with a golden snapshot; keyboard map works; a11y tree golden |
| `agent` | `crates/app/src/{mcp,agent_panel,commands/agent}.rs`, Settings ▸ AI | command contract, MCP schemas | every MCP command returns its golden JSON; limits and redaction applied; agent panel and `agent.undo`; `.ui` script test of an agent session |
| `bench` | `benches/*`, `docs/benchmarks.md` | a working app (week 3+) | scripts that generate files and measure every row of section 6 for Strata and the other apps; results reproducible |

### Worktrees

- One worktree per stream: `.claude/worktrees/<stream>` on branch `wt/<stream>`, created from
  `main` after the freeze. `.claude/worktrees/` is in `.gitignore`.
- Each sets `CARGO_TARGET_DIR=.claude/worktrees/<stream>/target` and runs heavy commands through
  `memgate`.
- Streams build and test only their crates (`cargo test -p strata-core`); the lead runs the full
  gate when merging.
- Nobody commits on `main` or pushes; the lead merges.

### Merge order

1. Contracts (lead)
2. `index`, then `formats`
3. `search`
4. `ui` and `agent` (either order; rebase the second)
5. `bench`

Each merge: rebase on `main`, full `cargo clippy --all-targets -- -D warnings` and `cargo test`,
then fast-forward.

### TEAM.md template

```markdown
# Team roster

Lead: <name> (freezes contracts, merges, runs the full gate). Nobody else merges or pushes.

| Agent | Stream | Worktree · branch | Now | Done (commits) |
|---|---|---|---|---|
| <name> | index | .claude/worktrees/index · wt/index | | |
| <name> | formats | .claude/worktrees/formats · wt/formats | | |
| <name> | search | .claude/worktrees/search · wt/search | | |
| <name> | ui | .claude/worktrees/ui · wt/ui | | |
| <name> | agent | .claude/worktrees/agent · wt/agent | | |
| <name> | bench | .claude/worktrees/bench · wt/bench | | |

## Log (append one line: time · from → to · what)
```

## 9. Risks

- **ashlar gaps.** A virtual table with resizable, reorderable columns and a custom histogram
  painter may need ashlar changes; those go into ashlar first (as general components), never as
  app code there.
- **Index memory on 50 M+ line files.** Mitigated by compressed chunk deltas; measured in week 1.
- **Follow on network filesystems** has no reliable file events; fall back to a 1 s poll only for
  those paths, shown in the status bar.
- **Redaction misses a secret.** Defaults are conservative; the agent panel shows exactly what was
  sent, so the person can check.
- **Windows file locking** while another process writes the log: open with share flags; tested on
  Windows CI.

## 10. Open questions

- Name check: is "Strata" free enough on crates.io, GitHub and app stores?
- License: BSD-3-Clause like ashlar, or Apache-2.0?
- Should merge-by-timestamp move into the MVP? It is the most-requested feature in similar tools.
- Paid tier later (team sharing of saved filters), or fully free as ashlar's showcase?
