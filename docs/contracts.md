# Strata contracts (M0, frozen)

What the six streams build on. Everything here compiles, has stub bodies (no `todo!`, no panics)
and is checked by `cargo xtask ci`. After the freeze a contract changes **additively only**, in its
own commit, announced to every stream that uses it and to the lead (TEAM.md log).

## Layout

```
crates/core/   strata-core: headless (no egui, no ashlar)
  src/lib.rs, types.rs, error.rs, limits.rs        lead
  src/{mmap,index,file,follow,gz}.rs               index
  src/formats/*, redact.rs, time.rs                formats
  src/{search,filter,histogram,stats}.rs           search
crates/app/    strata: lib (`strata::app()`), bins `strata` and `strata-cli`
  src/lib.rs, doc.rs, results.rs                   lead
  src/commands/{mod,ids}.rs                        lead
  src/commands/user.rs, views/*, scenes.rs (setups) ui
  src/commands/agent.rs, mcp.rs, agent_panel.rs    agent
  tests/mcp/*.json, tests/mcp_shapes.rs            lead (golden results)
  tests/smoke.rs                                   lead
xtask/         `cargo xtask ci`, `cargo xtask fixtures`
benches/       bench stream (fixtures land in benches/fixtures/, gitignored)
```

A stream fills in bodies in its own files and keeps every public signature. Need a new public
item in another stream's file? Ask that stream; it adds it (additive).

## 1. Commands (`crates/app/src/commands/`)

- `ids.rs`: every id as a constant with its `&[Param]` list. `ids::AGENT` is the MCP table
  (docs/PLAN.md §3), `ids::READ_ONLY` the agent commands that never change the view, `ids::USER`
  the user commands (menus, palette, keyboard).
- `mod.rs`: `commands()` registers each id with label, menu, shortcut, params, enabled predicate
  and handler. Read-only agent commands are `.query()` with no menu and the `always` predicate
  (what `query!` builds; the macros need literal ids, so the table uses `CommandSpec::new` with the
  constants).
- Handlers: `agent.rs` (agent stream) and `user.rs` (ui stream). Every stub returns
  `CommandError::failed(id, "not yet")`. Enabled predicates are functions in the same files
  (`agent::can_change_view`, `user::has_file`, …): fill them in there.
- Parameters: `file` is a file id (integer from `log.open`) and **defaults to the active file**
  everywhere, so shortcuts work with `{}`. `line` is 1-based. Times are RFC 3339 (no zone = UTC);
  durations `<int><ms|s|m|h|d>`; `filter` is the filter grammar below.
- ashlar's built-ins stay: `file.new/open/save/save_as/revert` work on the **workspace** (a
  `.strata` file), `app.undo/redo`, `app.palette` (`Mod+Shift+P`). Opening a log is
  `log.open_dialog` (`Mod+Shift+O`; with `{path}` it opens that file without asking) or
  `log.open {path}`. B is `bookmark.here` (the person's bookmark of the selected line); agents
  use `bookmark.add`.

## 2. Core types (`crates/core/src/`)

Conventions: line numbers are 0-based `u64` in the core and 1-based everywhere a person or agent
sees them (`strata::mcp::line_out` / `line_in`). Times are `Timestamp` = microseconds since the
epoch, UTC; shown as `Timestamp::to_rfc3339()` (`2026-11-12T07:41:03.118Z`). Long functions take
`cancel: &dyn Fn() -> bool` and return `Error::Cancelled`. One error type: `strata_core::Error`.

| Type / fn | File | Notes |
|---|---|---|
| `FileId`, `FieldId`, `Span`, `Timestamp`, `Level`, `LevelCounts`, `Parsed` | types.rs | `Level::as_str` words are the grammar's and MCP's |
| `Error` | error.rs | add variants only |
| caps (`SEARCH_MAX` 500, `MCP_TEXT_BYTES` 2 KB, `CHUNK_LINES` 65 536, …) | limits.rs | part of the MCP contract |
| `Mapped` | mmap.rs | the only module allowed `unsafe` (core's lints are `deny`, not `forbid`) |
| `LineIndex`, `ChunkSummary`, `IndexState` | index.rs | chunk summaries let filters skip chunks |
| `LogFile`, `Line`, `FileChange` | file.rs | shared as `Arc<LogFile>`, every method `&self`; `index_more` is called in a loop by the index job |
| `Watcher` | follow.rs | events, poll fallback on network filesystems |
| `gz::decompress`, `gz::is_gzip` | gz.rs | |
| `FormatKind`, `detect`, `Parser`, `parser`, `FieldNames` | formats/mod.rs | ids `plain jsonl logfmt syslog timestamped access` |
| `Detection`, `detect_scored`, `Parser::parse_into`, `level_word`, `level_number`, one `*Parser` per format | formats/* | `parse_into` reuses a `Parsed`; caps `MAX_FIELD_NAMES` 4096, `MAX_FIELD_NAME_BYTES` 128, `MAX_FIELDS_PER_LINE` 256, `JSON_FLATTEN_DEPTH` 4; `FieldNames::try_intern` is `None` past the caps, `intern` gives `FieldId(u32::MAX)` |
| `Redaction`, `RedactKind`, `Pattern` | redact.rs | replacement `[redacted:<kind>]`; kinds also `api-key credit-card secret-field ipv4 ipv6` (`RedactKind::BUILTIN`, applied in that order); `Redaction.disabled` (serde default: the IPs) with `is_on`/`set`; `Pattern::check` |
| `time::parse_time`, `parse_duration`, `format_duration` | time.rs | durations also `us` and a sign |
| `time::scan_timestamp`, `scan_timestamp_near`, `epoch_number`, `days_from_civil` | time.rs | log timestamps at the start of bytes → UTC µs; `_near` infers syslog's year from a given time |
| `Query`, `Matcher`, `Match`, `search`, `find_next`, `count` | search.rs | |
| `Filter`, `Field`, `CmpOp`, `Literal` | filter.rs | text grammar in the module docs; `parse(to_text(f)) == f` |
| `Histogram`, `Bucket`, `histogram`, `auto_width` | histogram.rs | |
| `FileStats`, `FieldSummary`, `FieldType`, `stats`, `fields` | stats.rs | |

### Filter grammar (frozen)

```text
filter := or            or := and ("or" and)*          and := unary ("and"? unary)*
unary  := ("not" | "-") unary | "(" filter ")" | term
term   := field op value | "/" regex "/" "i"? | string
field  := level | time | message | line | <name>          (dots allowed: http.status)
op     := = != < <= > >= ~ (contains, ignore case) =~ (regex)
```

`level >= warn` compares severity; bare or quoted strings search the line ignoring case; `line` is
1-based. Examples: `level >= warn and service = api`, `status >= 500 -"healthz"`,
`time >= 2026-11-12T14:02:00Z`, `/time(d)?out/i`.

## 3. MCP results (`crates/app/src/results.rs`, `crates/app/tests/mcp/`)

One type per agent command (`Open`, `Files`, `Stats`, `Search`, `Lines`, `Around`,
`HistogramOut`, `Fields`, `First`, `Filtered`, `Cleared`, `Goto`, `Highlighted`, `BookmarkAdded`,
`Bookmarks`, `Noted`, `Undone`), each with `example()`. `tests/mcp/<id>.json` is that example
serialized: the golden result. Handlers return `serde_json::to_value(<type>)`.

`tests/mcp_shapes.rs` checks (a) one golden file per agent command, (b) the types still serialize
to the golden shape, (c) whatever a command returns through a headless session has the golden
shape: same keys, same JSON types (integer vs fraction), arrays shaped like the golden's first
element, `null` allowed anywhere. So a stream filling in values never changes a shape by accident.

Result conventions: 1-based lines, RFC 3339 times or `null`, level words or `null`, every list
capped with an `omitted` count, line text redacted and cut to 2 KB (`cut: true`). The error when
the person turned agent changes off is `results::DENIED`.

## 4. Document (`crates/app/src/doc.rs`)

`StrataDoc { files: Vec<OpenFile>, next_id, saved_filters, agent: AgentState, live: Live }`.
`OpenFile` holds the path, follow, format override, filter chips, hidden levels, time range,
columns, bookmarks, highlights and per-file redaction. Lines are stored 0-based. Ids for files,
chips, bookmarks, highlights and notes come from one counter (`take_id`). `live` is the runtime
table of open `Arc<LogFile>`s (`#[serde(skip)]`, shared by every revision, ignored by `==`).
`AgentState.pending` lists `AgentChange`s since the person last acted: what `agent.undo` removes.

Undo labels: `doc::labels` (one constant per kind of change, `labels::ALL`). View state that is
not undoable (active file, selection, panels, search, agent timeline) uses the keys in
`doc::view_keys`.

## 5. Scenes and the app

`scenes::NAMES` (frozen): `empty`, `indexing`, `main`, `agent`, `palette`, `settings`,
`no-matches`, `file-replaced`, `unreadable`, `agent-disconnected`. The ui stream fills
`scenes::setup` with fixtures. `strata --headless --scene main --snap out.png --tree tree.json`
renders one.

## Changing a contract

1. Add, don't change: a new command id, param, result field, enum variant (enums are
   `#[non_exhaustive]` where streams match on them), doc field with `#[serde(default)]`.
2. Update `results.rs` `example()` and regenerate the golden files:
   `STRATA_UPDATE_GOLDEN=1 cargo test -p strata --test mcp_shapes`.
3. Update docs/PLAN.md §3 if the MCP surface changed, and this file.
4. Log it in TEAM.md and tell the streams that use it.

## Test data

`cargo xtask fixtures --size 100M --format jsonl [--seed 42] [--out PATH]` writes realistic,
deterministic logs (`jsonl`, `logfmt`, `plain` with stack traces, `syslog` RFC 5424, `access`),
with error bursts and a few secrets for redaction tests. Default path:
`benches/fixtures/<format>-<bytes>-s<seed>.<ext>` (gitignored).
