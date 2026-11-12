# Team roster

Lead: the owner (freezes contracts, merges, runs the full gate). Nobody else merges or pushes.

Contracts (M0) are on `wt/contracts`; read docs/contracts.md before starting. Streams branch from
`main` after it is merged.

| Agent | Stream | Worktree · branch | Now | Done (commits) |
|---|---|---|---|---|
| | index | .claude/worktrees/index · wt/index | | |
| formats agent | formats | .claude/worktrees/formats · wt/formats | done, ready to merge | STR-FMT.1 |
| | search | .claude/worktrees/search · wt/search | | |
| | ui | .claude/worktrees/ui · wt/ui | | |
| | agent | .claude/worktrees/agent · wt/agent | | |
| | bench | .claude/worktrees/bench · wt/bench | | |

## Log (append one line: time · from → to · what)

- 2026-11-12 · contracts → all · M0 contracts frozen on wt/contracts (docs/contracts.md)
- 2026-11-12 · formats → lead, index, search, agent · additive contract changes on wt/formats: `formats::{Detection, detect_scored, level_word, level_number, MAX_FIELD_NAMES, MAX_FIELD_NAME_BYTES, MAX_FIELDS_PER_LINE, JSON_FLATTEN_DEPTH, DETECT_LINES}`, one parser type per format, `Parser::parse_into` (default method, reuses `Parsed`), `FieldNames::try_intern` (`intern` gives `FieldId(u32::MAX)` past the caps); `time::{scan_timestamp, scan_timestamp_near, epoch_number, days_from_civil}`, `parse_duration` also reads `us` and a sign; `RedactKind::{ApiKey, CreditCard, SecretField, Ipv4, Ipv6}` + `BUILTIN`, `on_by_default`; `Redaction.disabled` (`#[serde(default)]`, IPs off), `Redaction::{is_on, set}`, `Pattern::check`. New deps in strata-core: `memchr`, `regex` (search will want the same), dev `proptest` (Cargo.lock changed)
