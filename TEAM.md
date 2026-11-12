# Team roster

Lead: the owner (freezes contracts, merges, runs the full gate). Nobody else merges or pushes.

Contracts (M0) are on `wt/contracts`; read docs/contracts.md before starting. Streams branch from
`main` after it is merged.

| Agent | Stream | Worktree · branch | Now | Done (commits) |
|---|---|---|---|---|
| claude | index | .claude/worktrees/index · wt/index | done; ready to merge | STR-IDX.1 mmap + line index + LogFile, STR-IDX.2 follow, STR-IDX.3 gz |
| formats agent | formats | .claude/worktrees/formats · wt/formats | merged to main | STR-FMT.1 |
| claude (search) | search | .claude/worktrees/search · wt/search | done; rebased on main, integrated with index + formats | STR-SRCH.1, STR-SRCH.2 |
| | ui | .claude/worktrees/ui · wt/ui | | |
| | agent | .claude/worktrees/agent · wt/agent | | |
| | bench | .claude/worktrees/bench · wt/bench | | |

## Log (append one line: time · from → to · what)

- 2026-11-12 · contracts → all · M0 contracts frozen on wt/contracts (docs/contracts.md)
- 2026-11-12 · formats → lead, index, search, agent · additive contract changes on wt/formats: `formats::{Detection, detect_scored, level_word, level_number, MAX_FIELD_NAMES, MAX_FIELD_NAME_BYTES, MAX_FIELDS_PER_LINE, JSON_FLATTEN_DEPTH, DETECT_LINES}`, one parser type per format, `Parser::parse_into` (default method, reuses `Parsed`), `FieldNames::try_intern` (`intern` gives `FieldId(u32::MAX)` past the caps); `time::{scan_timestamp, scan_timestamp_near, epoch_number, days_from_civil}`, `parse_duration` also reads `us` and a sign; `RedactKind::{ApiKey, CreditCard, SecretField, Ipv4, Ipv6}` + `BUILTIN`, `on_by_default`; `Redaction.disabled` (`#[serde(default)]`, IPs off), `Redaction::{is_on, set}`, `Pattern::check`. New deps in strata-core: `memchr`, `regex` (search will want the same), dev `proptest` (Cargo.lock changed)
- 2026-11-12 · index → lead, formats, search · additive only: new pub items `LogFile::{index_all, summarize_chunk}`, `LineIndex::{raw_range, ends_in_cr, apply}`, `index::{scan, Found, GROUP}`, `Mapped::{file, file_len, valid_len, read_at, release}`, `follow::is_remote`, `file::looks_binary`; `with_bytes` passes lines with their newlines and holds the map read lock (call per chunk); new deps in crates/core/Cargo.toml (memmap2, memchr, notify with `macos_kqueue`, same-file, flate2, rustix on unix; dev: tempfile, proptest)
- 2026-11-12 · search → lead, index, formats, agent, ui · additive: `search::{LineSource, MemoryLog, search_in, find_next_in, count_in}`, `Matcher::{all, is_all}`, `Filter::{compile, normalize}` + `filter::CompiledFilter`, `Field::as_str`, `histogram::histogram_in`, `stats::{stats_in, fields_in}`, limit consts; deps `regex`, `memchr` (+ dev `proptest`). Needs from index: `LogFile::with_bytes` callable from many threads at once (no exclusive lock while `f` runs), `with_index(|ix| ix.chunk(i))` with `first_line = i * CHUNK_LINES`.
