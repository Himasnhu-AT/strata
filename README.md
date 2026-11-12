# Strata

**A log viewer for huge files that you and your AI agent can read together.**

Open a 5 GB log and start scrolling before it finishes indexing. Search with regex, filter by level,
time or any parsed field, follow a live file, and jump from error to error. Ordinary text logs like

```
2026-11-12 10:00:01.513 ERROR [payments] db timeout after 5s order=8812 user=u-3301
java.net.SocketTimeoutException: db timeout
	at com.acme.payments.db.Pool.acquire(Pool.java:212)
```

are split into time, level, service, message and `key=value` fields automatically, with stack
traces kept under their entry; JSON lines, logfmt, syslog and access logs are detected the same
way, and `.gz` files open too. Strata only reads: your logs stay plain files you can open in any
editor.

Try it on a generated log: `cargo xtask fixtures --size 50M` writes a realistic plain-text `.log`
(`--format jsonl|logfmt|syslog|access` for the others).

Every action is a command, so an AI agent (Claude, Cursor, any MCP client) can drive Strata too: it
runs searches, narrows the time range, highlights the lines that matter and leaves bookmarks, while
you watch the same window. Everything the agent did is listed in a side panel and can be undone in
one step. Nothing leaves your machine unless you send it.

Built in Rust on [ashlar](https://github.com/Himasnhu-AT/ashlar). No Electron, no web view; an idle
window does no work.

> **Status:** planning. Development starts after ashlar 0.2 is released.

- [Plan](docs/PLAN.md): features, MCP commands, architecture, benchmarks, milestones
- [Design](docs/DESIGN.md): screens, interactions, keyboard map
- [Contracts](docs/contracts.md): the frozen command, type and MCP contracts the streams build on

License: to be decided before the first release (BSD-3-Clause, like ashlar, is the default choice).
