//! Building agent results: limits, time budgets, redaction, attribution, the agent timeline and
//! the 1-based conversions between strata-core and the `crate::results` types. Owned by the
//! `agent` stream.
//!
//! # Where the work runs (ashlar 0.2)
//!
//! A command body cannot start a job in ashlar 0.2, so agent commands do their index and search
//! work inline, bounded by a time budget ([`budget`]): [`BUDGET_HEADLESS`] in `strata-cli mcp` /
//! `exec` (no window, nothing else to keep responsive) and [`BUDGET_WINDOWED`] in the window,
//! where commands run on the UI thread. A search or count that runs out of budget returns what it
//! found with `complete: false`. In the window, the view's index job does the bulk of indexing;
//! `log.open` only indexes the first [`OPEN_WINDOWED_BYTES`] so the first screen shows at once.
//! Every agent command first catches the index up for part of its budget ([`catch_up`]), so
//! without a window a big file indexes over a few calls and results say `complete: false` until
//! it is done.
//!
//! # Attribution (ashlar 0.2)
//!
//! A handler cannot tell an agent from the person before it edits (`Session::origin` only reports
//! a change after it is committed), so attribution is by command: the agent commands are the
//! agent's ([`by_agent`]). Two of them also have a menu item or shortcut for the person
//! (`bookmark.add` on `B`, `view.clear_filters` in View): in a window, those calls with no
//! parameters at all (what a shortcut or menu sends) count as the person's.

use std::borrow::Cow;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ashlar::prelude::*;
use serde::{Deserialize, Serialize};
use strata_core::index::IndexState;
use strata_core::limits::{CHUNK_LINES, MAX_LINE_BYTES, MCP_TEXT_BYTES, cut};
use strata_core::{Level, LevelCounts, Line, LineNo, LogFile, Redaction, Timestamp};

use crate::commands::S;
use crate::doc::{OpenFile, view_keys};
use crate::results::{FieldValue, Hit, Indexing, Levels, LineOut, TimeSpan};

/// How long an agent command may work when there is no window (CLI, `strata-cli mcp`).
pub const BUDGET_HEADLESS: Duration = Duration::from_secs(5);
/// How long an agent command may work in the window (it runs on the UI thread).
pub const BUDGET_WINDOWED: Duration = Duration::from_millis(250);
/// `log.open` in a window indexes this much inline; the view's index job does the rest.
pub const OPEN_WINDOWED_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes indexed per step while catching up.
pub const INDEX_STEP: u64 = 16 * 1024 * 1024;
/// A `.gz` file is decompressed to a temporary file of at most this size.
pub const GZ_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Rows kept in the agent timeline (`view_keys::AGENT_TIMELINE`).
pub const TIMELINE_MAX: usize = 500;

/// View-state keys of the agent stream (not undoable, not saved; mirrored from Settings ▸ AI).
pub mod keys {
    /// [`strata_core::Redaction`] as JSON: the redaction applied to agent results (per-kind
    /// switches and custom patterns); absent: the defaults. A file with redaction turned off
    /// (`OpenFile::redact`, `log.redaction`) is not redacted at all.
    pub const REDACTION: &str = "agent_redaction";
}

/// Settings keys of Settings ▸ AI (ashlar app settings, saved in the config folder).
pub mod settings_keys {
    /// `bool`: "Allow the agent to change the view" when the app starts (default true).
    pub const ALLOW_CHANGES: &str = "ai.allow_changes";
    /// [`strata_core::Redaction`]: redaction of agent results.
    pub const REDACTION: &str = "ai.redaction";
}

/// A core (0-based) line number as agents see it (1-based).
pub fn line_out(line: LineNo) -> u64 {
    line.saturating_add(1)
}

/// An agent's (1-based) line number as the core's (0-based); `None` for 0.
pub fn line_in(line: u64) -> Option<LineNo> {
    line.checked_sub(1)
}

/// A time as agents see it.
pub fn time_out(t: Option<Timestamp>) -> Option<String> {
    t.map(Timestamp::to_rfc3339)
}

/// The time budget of a command in this session.
pub fn budget(s: &S) -> Duration {
    if s.is_windowed() { BUDGET_WINDOWED } else { BUDGET_HEADLESS }
}

/// A point in time after which work stops (`complete: false`).
#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn after(d: Duration) -> Deadline {
        Deadline(Instant::now() + d)
    }

    /// The cancel check strata-core takes.
    pub fn passed(&self) -> bool {
        Instant::now() >= self.0
    }
}

/// Index more of `file` until it is complete or `deadline` passes (and pick up appended bytes
/// when it is followed), then summarize the chunks that are full (or all, once complete) so
/// filters and the histogram can skip them.
pub fn catch_up(file: &LogFile, follow: bool, deadline: Deadline) {
    if follow {
        let _ = file.refresh();
    }
    loop {
        match file.index_state() {
            IndexState::Complete if !follow => break,
            IndexState::Failed(_) | IndexState::Cancelled => break,
            _ => {}
        }
        if deadline.passed() {
            return;
        }
        match file.index_more(INDEX_STEP) {
            Ok(IndexState::Running { .. }) => {}
            _ => break,
        }
    }
    let complete = is_complete(file);
    let chunks = file.with_index(|ix| ix.chunks());
    for i in 0..chunks {
        if deadline.passed() {
            return;
        }
        let Some(c) = file.with_index(|ix| ix.chunk(i)) else { break };
        let full = c.lines as usize >= CHUNK_LINES;
        if !c.summarized && (full || complete) {
            let _ = file.summarize_chunk(i, &|| deadline.passed());
        }
    }
}

/// Where indexing of `file` is, as agents see it.
pub fn indexing_out(file: &LogFile) -> Indexing {
    let len = file.len();
    let indexed = file.with_index(|ix| ix.indexed_bytes());
    match file.index_state() {
        IndexState::Running { done, total } => Indexing { state: "running".into(), done_bytes: done, total_bytes: total, error: None },
        IndexState::Failed(e) => Indexing { state: "failed".into(), done_bytes: indexed, total_bytes: len, error: Some(e) },
        st => Indexing { state: st.as_str().into(), done_bytes: indexed.min(len), total_bytes: len, error: None },
    }
}

/// Whether every byte of `file` is indexed.
pub fn is_complete(file: &LogFile) -> bool {
    file.index_state() == IndexState::Complete
}

/// Line counts per level as agents see them.
pub fn levels_out(c: &LevelCounts) -> Levels {
    let g = |l: Level| c.get(l);
    Levels {
        trace: g(Level::Trace),
        debug: g(Level::Debug),
        info: g(Level::Info),
        warn: g(Level::Warn),
        error: g(Level::Error),
        fatal: g(Level::Fatal),
        none: c.none,
    }
}

/// The time span of `file`: from the chunk summaries when every chunk has one, else the first and
/// last timestamps among the first and last lines.
pub fn time_span(file: &LogFile) -> Option<TimeSpan> {
    let (all, min, max) = file.with_index(|ix| {
        let mut all = ix.chunks() > 0;
        let (mut min, mut max): (Option<Timestamp>, Option<Timestamp>) = (None, None);
        for i in 0..ix.chunks() {
            let Some(c) = ix.chunk(i) else { continue };
            all &= c.summarized;
            if let Some(t) = c.min_time {
                min = Some(min.map_or(t, |m| m.min(t)));
            }
            if let Some(t) = c.max_time {
                max = Some(max.map_or(t, |m| m.max(t)));
            }
        }
        (all, min, max)
    });
    let (from, to) = if all {
        (min?, max?)
    } else {
        const PROBE: usize = 64;
        let n = file.lines();
        let first = file.lines_from(0, PROBE, MAX_LINE_BYTES).iter().find_map(|l| file.parse(&l.bytes).time)?;
        let tail = file.lines_from(n.saturating_sub(PROBE as u64), PROBE, MAX_LINE_BYTES);
        let last = tail.iter().rev().find_map(|l| file.parse(&l.bytes).time).unwrap_or(first);
        (first, last)
    };
    Some(TimeSpan { from: from.to_rfc3339(), to: to.to_rfc3339() })
}

/// The redaction agent results of `of` pass through: off when the person turned it off for the
/// file, else Settings ▸ AI's (the defaults when unset).
pub fn redaction(s: &S, of: &OpenFile) -> Redaction {
    if !of.redact {
        return Redaction::off();
    }
    s.view::<Redaction>(keys::REDACTION).map(|r| Redaction { enabled: true, ..r }).unwrap_or_default()
}

/// Text on its way to an agent: redacted, cut to [`MCP_TEXT_BYTES`], with the number of
/// replacements counted (the agent panel shows them).
#[derive(Debug)]
pub struct Out {
    red: Redaction,
    /// Secrets replaced so far.
    pub redacted: u64,
}

impl Out {
    pub fn new(red: Redaction) -> Out {
        Out { red, redacted: 0 }
    }

    fn redact<'a>(&mut self, text: &'a str) -> Cow<'a, str> {
        let (r, hits) = self.red.redact_counted(text);
        self.redacted = self.redacted.saturating_add(hits.iter().map(|(_, n)| u64::from(*n)).sum::<u64>());
        r
    }

    /// Redact `text` and cut it to [`MCP_TEXT_BYTES`]; whether it was cut.
    pub fn text(&mut self, text: &str) -> (String, bool) {
        let r = self.redact(text);
        let (c, was_cut) = cut(&r, MCP_TEXT_BYTES);
        (c.to_string(), was_cut)
    }

    /// A field value, redacted with its name as context (`password` = `hunter2` is a secret even
    /// though `hunter2` alone is not), cut to [`MCP_TEXT_BYTES`].
    pub fn field(&mut self, name: &str, value: &str) -> String {
        let joined = format!("{name}={value}");
        let v: String = match self.redact(&joined) {
            Cow::Borrowed(_) => value.to_string(),
            Cow::Owned(o) => match o.strip_prefix(&format!("{name}=")) {
                Some(rest) => rest.to_string(),
                None => o,
            },
        };
        cut(&v, MCP_TEXT_BYTES).0.to_string()
    }

    /// A line with its parsed fields. `line` was read with [`MAX_LINE_BYTES`].
    pub fn line(&mut self, file: &LogFile, line: &Line) -> LineOut {
        let parsed = file.parse(&line.bytes);
        let parser = strata_core::formats::parser(file.format());
        let (text, text_cut) = self.text(&line.text());
        // the message and fields repeat parts of the text: count each secret once
        let counted = self.redacted;
        let message = parsed.message.slice(&line.bytes).map(String::from_utf8_lossy).unwrap_or_default();
        let (message, _) = self.text(&message);
        let names = file.names();
        let fields = parsed
            .fields
            .iter()
            .map(|(id, span)| {
                let name = names.name(*id).unwrap_or_default();
                let value = parser.field_text(&line.bytes, *span);
                FieldValue { value: self.field(&name, &value), name }
            })
            .collect();
        self.redacted = counted;
        LineOut {
            line: line_out(line.number),
            time: time_out(parsed.time),
            level: parsed.level.map(|l| l.as_str().to_string()),
            message,
            text,
            cut: line.cut || text_cut,
            fields,
        }
    }

    /// A search hit.
    pub fn hit(&mut self, file: &LogFile, line: &Line) -> Hit {
        let parsed = file.parse(&line.bytes);
        let (text, text_cut) = self.text(&line.text());
        Hit {
            line: line_out(line.number),
            time: time_out(parsed.time),
            level: parsed.level.map(|l| l.as_str().to_string()),
            text,
            cut: line.cut || text_cut,
        }
    }
}

/// Who made a call of an agent command: the agent, except a parameterless call of a command the
/// person also has a shortcut or menu item for, in a window (see the module docs).
pub fn by_agent(s: &S, p: &Value, has_user_binding: bool) -> bool {
    let empty = p.as_object().is_none_or(|m| m.is_empty());
    !(s.is_windowed() && has_user_binding && empty)
}

/// One row of the agent timeline (`view_keys::AGENT_TIMELINE`), newest last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimelineRow {
    pub command: String,
    /// "Searched `timeout` · 312 matches".
    pub summary: String,
    pub changed_view: bool,
    /// RFC 3339.
    pub at: String,
    /// The file and 1-based line to jump to from the row (when there is one).
    #[serde(default)]
    pub file: Option<u64>,
    #[serde(default)]
    pub line: Option<u64>,
    /// What the call added (filter chip, highlight, bookmark or note id).
    #[serde(default)]
    pub item: Option<u64>,
    /// Secrets redacted in what was sent back.
    #[serde(default)]
    pub redacted: u64,
    /// The call failed (the summary says why).
    #[serde(default)]
    pub failed: bool,
}

/// The agent timeline.
pub fn timeline(s: &S) -> Vec<TimelineRow> {
    s.view::<Vec<TimelineRow>>(view_keys::AGENT_TIMELINE).unwrap_or_default()
}

/// Add a row to the agent timeline (dropping the oldest past [`TIMELINE_MAX`]); a call that
/// changed the view also opens the agent panel.
pub fn record(s: &mut S, mut row: TimelineRow) {
    if row.at.is_empty() {
        row.at = now().to_rfc3339();
    }
    if row.changed_view && !row.failed {
        s.set_view(view_keys::AGENT_PANEL_OPEN, &true);
    }
    let mut rows = timeline(s);
    rows.push(row);
    let over = rows.len().saturating_sub(TIMELINE_MAX);
    rows.drain(..over);
    s.set_view(view_keys::AGENT_TIMELINE, &rows);
}

/// The wall clock.
pub fn now() -> Timestamp {
    let micros = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX)).unwrap_or(0);
    Timestamp(micros)
}

/// `n` with thousands separators: `1,284,113`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `"1 line"`, `"3 lines"`.
pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_one_based_outside() {
        assert_eq!(line_out(0), 1);
        assert_eq!(line_in(1), Some(0));
        assert_eq!(line_in(0), None);
    }

    #[test]
    fn counts_have_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_284_113), "1,284,113");
        assert_eq!(plural(1, "line", "lines"), "1 line");
    }

    #[test]
    fn field_values_are_redacted_with_their_name() {
        let mut o = Out::new(Redaction::default());
        assert_eq!(o.field("password", "hunter2"), "[redacted:secret-field]");
        assert_eq!(o.field("service", "api"), "api");
        assert_eq!(o.field("user", "ana@example.com"), "[redacted:email]");
        assert_eq!(o.redacted, 2);
        let mut off = Out::new(Redaction::off());
        assert_eq!(off.field("password", "hunter2"), "hunter2");
    }

    #[test]
    fn text_is_redacted_then_cut() {
        let mut o = Out::new(Redaction::default());
        let long = format!("token=abc123secret {}", "x".repeat(4000));
        let (t, c) = o.text(&long);
        assert!(c && t.len() <= MCP_TEXT_BYTES && !t.contains("abc123secret"), "{t}");
    }
}
