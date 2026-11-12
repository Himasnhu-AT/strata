# Strata: rules for agents and developers

Strata is a desktop log viewer built on ashlar (`ashlar-rs` 0.2 from crates.io, imported as
`ashlar`). Read docs/PLAN.md and docs/DESIGN.md before starting a task.

## Never crash
- No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` or `unimplemented!` outside tests
  (denied in `[lints.clippy]`). No `unsafe`, except the one module that maps files, which keeps
  every `unsafe` block small with a `// SAFETY:` comment.
- Log files are hostile input: any length, any bytes, any encoding, truncated mid-line, growing
  or shrinking while open. Use `get()` not `[i]`, checked math and capped allocations; a line
  longer than the cap is shown cut, never loaded whole.
- No blocking I/O on the UI thread: opening, indexing, searching and parsing run as ashlar jobs
  with progress and cancellation.

## Every action is a command
- Anything a user can do is a command registered with ashlar (`cmd!` / `CommandSpec`), with typed
  params, so the menus, palette, CLI and MCP get it for free. Views dispatch commands; they don't
  change state directly.
- Commands an agent can call are documented in docs/PLAN.md (the MCP table). Changing one is a
  contract change: update the table in the same commit.
- Agent actions that change what the user sees (filters, highlights, bookmarks) are undoable and
  listed in the agent panel.

## Privacy
- Strata never sends log text anywhere by itself. Text reaches an agent only through an MCP call
  the user connected, and passes through the redaction filter first (docs/PLAN.md, Privacy).

## Checks
- `cargo clippy --all-targets -- -D warnings` and `cargo test` pass before every commit.
- UI changes: render with `--headless --snap out.png --tree tree.json` and look at the result.
- Heavy commands (builds, tests, benchmarks) run through `memgate`: `memgate -- cargo test`.

## Git and parallel work
- One git worktree per agent: `.claude/worktrees/<name>` on branch `wt/<name>`, with its own
  `CARGO_TARGET_DIR=.claude/worktrees/<name>/target`. Stay inside your workstream's files
  (docs/PLAN.md, Parallel work); ask the stream that owns a file to change it.
- Never commit on `main` and never push; the lead merges branches in the order the plan gives.
- The shared contracts (command ids and params, data types in `strata-core`, MCP tool schemas)
  are frozen first. Change one only additively, and tell every stream that uses it and the lead.
- Keep your row in TEAM.md current.
- One task id per commit (`STR-1.2: line index job`), only green states.
