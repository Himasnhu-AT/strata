//! Handlers of the agent (MCP) commands. Owned by the `agent` stream: fill in the bodies, keep
//! the signatures (they are registered in `commands/mod.rs`) and return the `crate::results`
//! types. View-changing commands record an `AgentChange` when the caller is an agent, and fail
//! with `crate::results::DENIED` when the person turned agent changes off.
//!
//! Every handler follows the same steps: find the file (`file` defaults to the active one), catch
//! its index up for part of the time budget (`crate::mcp::catch_up`), do the work within the rest
//! of the budget, pass every line of text through the redaction filter (`crate::mcp::Out`), and,
//! when the agent made the call, add a row to the agent timeline (`crate::mcp::record`). Where the
//! work runs and how calls are attributed is in the `crate::mcp` docs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ashlar::prelude::*;
use serde::Serialize;
use strata_core::limits::{
    AROUND_DEFAULT, AROUND_MAX, FIELD_VALUES, FIELDS_MAX, HIGHLIGHT_MAX, HISTOGRAM_MAX, LINES_MAX, LIST_MAX, MAX_LINE_BYTES, NOTE_MAX_BYTES,
    SEARCH_DEFAULT, SEARCH_MAX, STATS_TOP_FIELDS, STATS_TOP_VALUES,
};
use strata_core::{CmpOp, Error, Field, FileId, Filter, Level, LineNo, Literal, LogFile, Query, Timestamp, gz, histogram, search, stats, time};

use super::{R, S, ids};
use crate::doc::{
    Actor, AgentChange, AgentNote, Bookmark, BookmarkColor, FilterChip, Highlight, OpenFile, StrataDoc, default_columns, labels, view_keys,
};
use crate::mcp::{self, Deadline, Out, TimelineRow, line_in, line_out, plural};
use crate::results::{
    BookmarkAdded, BookmarkOut, Bookmarks, BucketOut, Cleared, DENIED, FieldOut, Fields, FileInfo, Files, Filtered, First, Goto, Highlighted,
    HistogramOut, Indexing, Lines, Noted, Open, Search, Stats, TopField, Undone, ValueCount,
};

const NO_FILE: &str = "no log is open: open one with log.open {\"path\": …}";

/// Enabled predicate of the view-changing agent commands: off when the person turned off
/// "Allow the agent to change the view" (`view_keys::AGENT_ALLOW_CHANGES`).
///
/// Without a window every call is the agent's, so the switch applies here. In the window,
/// `bookmark.add` and `view.clear_filters` are also the person's (shortcut, menu), so the
/// handlers check the switch once they know who called ([`crate::mcp::by_agent`]).
pub fn can_change_view(s: &S) -> Result<(), String> {
    if s.is_windowed() || s.view_or(view_keys::AGENT_ALLOW_CHANGES, true) { Ok(()) } else { Err(DENIED.to_string()) }
}

/// Enabled predicate of `agent.undo`: there are agent changes since the person last acted.
pub fn has_agent_changes(s: &S) -> Result<(), String> {
    if s.doc().agent.pending.is_empty() { Err("no agent changes to undo since you last acted".to_string()) } else { Ok(()) }
}

/// The person acted: the agent's changes so far are no longer undone by `agent.undo` (they are
/// part of what the person built on). Call inside the edit of every person's command that
/// changes the document (the `ui` stream's handlers).
pub fn person_acted(doc: &mut StrataDoc) {
    doc.agent.pending.clear();
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn fail(id: &str, msg: impl Into<String>) -> CommandError {
    CommandError::failed(id, msg)
}

fn core(id: &str, e: Error) -> CommandError {
    fail(id, e.to_string())
}

fn str_param<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

fn u64_param(p: &Value, k: &str) -> Option<u64> {
    p.get(k).and_then(Value::as_u64)
}

fn bool_param(p: &Value, k: &str) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// The file shown in the window, else the most recently opened one.
fn active(s: &S) -> Option<FileId> {
    s.view::<u64>(view_keys::ACTIVE_FILE).map(FileId).filter(|f| s.doc().file(*f).is_some()).or_else(|| s.doc().files.last().map(|f| f.id))
}

/// Open `path` as file `id`; a `.gz` file is decompressed to a temporary file first.
fn open_log(id: FileId, path: &Path) -> strata_core::Result<LogFile> {
    let mut head = [0u8; 2];
    let gzip = std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .map(|n| gz::is_gzip(head.get(..n).unwrap_or_default()))
        .map_err(|e| Error::io(path, &e))?;
    if !gzip {
        return LogFile::open(id, path);
    }
    let dst = std::env::temp_dir().join(format!("strata-{}-{}.log", std::process::id(), id.0));
    gz::decompress(path, &dst, mcp::GZ_MAX_BYTES, &mut |_, _| true)?;
    LogFile::open(id, &dst)
}

/// The open `LogFile` of `of`, reopened from its path after a workspace restore.
fn live(s: &S, of: &OpenFile) -> strata_core::Result<Arc<LogFile>> {
    if let Some(f) = s.doc().live.get(of.id) {
        return Ok(f);
    }
    let f = Arc::new(open_log(of.id, &of.path)?);
    s.doc().live.insert(Arc::clone(&f));
    Ok(f)
}

/// What a command works on: the file, its live index and the deadline for the work.
struct Target {
    of: OpenFile,
    lf: Arc<LogFile>,
    work: Deadline,
}

impl Target {
    fn id(&self) -> u64 {
        self.of.id.0
    }
}

/// The file of `p` (`file`, default the active one), with its index caught up for half the budget.
fn target(s: &S, p: &Value, id: &str) -> Result<Target, CommandError> {
    let fid = match u64_param(p, "file") {
        Some(f) => FileId(f),
        None => active(s).ok_or_else(|| fail(id, NO_FILE))?,
    };
    let of = s.doc().file(fid).cloned().ok_or_else(|| fail(id, format!("no open file has id {} (log.files lists them)", fid.0)))?;
    let lf = live(s, &of).map_err(|e| core(id, e))?;
    let budget = mcp::budget(s);
    mcp::catch_up(&lf, of.follow, Deadline::after(budget / 2));
    Ok(Target { of, lf, work: Deadline::after(budget / 2) })
}

fn out(s: &S, t: &Target) -> Out {
    Out::new(mcp::redaction(s, &t.of))
}

/// A filter param (absent or blank: none).
fn filter_param(p: &Value, id: &str) -> Result<Option<Filter>, CommandError> {
    match str_param(p, "filter").map(str::trim).filter(|f| !f.is_empty()) {
        None => Ok(None),
        Some(text) => Filter::parse(text).map(Some).map_err(|e| core(id, e)),
    }
}

/// What the timeline shows for a call.
#[derive(Default)]
struct Row {
    summary: String,
    file: Option<u64>,
    line: Option<u64>,
    item: Option<u64>,
    redacted: u64,
}

/// Serialize the result and, when the agent called, add the timeline row.
fn done<T: Serialize>(s: &mut S, id: &str, agent: bool, changed_view: bool, row: Row, result: &T) -> R {
    let v = serde_json::to_value(result).map_err(|e| fail(id, e.to_string()))?;
    if agent {
        let summary =
            if row.redacted > 0 { format!("{} · {} redacted", row.summary, plural(row.redacted, "secret", "secrets")) } else { row.summary };
        mcp::record(
            s,
            TimelineRow {
                command: id.to_string(),
                summary,
                changed_view,
                file: row.file,
                line: row.line,
                item: row.item,
                redacted: row.redacted,
                ..TimelineRow::default()
            },
        );
    }
    Ok(v)
}

/// Fail with `DENIED` when the agent called and the person turned agent changes off.
fn check_allowed(s: &S, id: &str, agent: bool) -> Result<(), CommandError> {
    if agent && !s.view_or(view_keys::AGENT_ALLOW_CHANGES, true) { Err(fail(id, DENIED)) } else { Ok(()) }
}

fn actor(agent: bool) -> Actor {
    if agent { Actor::Agent } else { Actor::User }
}

/// The 0-based line `n` (1-based outside) of `t`, checked against what is indexed.
fn line_param(t: &Target, n: u64, id: &str) -> Result<LineNo, CommandError> {
    let total = t.lf.lines();
    match line_in(n) {
        Some(l) if l < total => Ok(l),
        _ => Err(fail(id, format!("line {n} is past the end ({} indexed)", plural(total, "line", "lines")))),
    }
}

/// The first line at or after 0 that passes `f`.
fn first_match(lf: &LogFile, f: &Filter, work: Deadline) -> strata_core::Result<Option<LineNo>> {
    if let Some(l) = lf.line(0, MAX_LINE_BYTES) {
        let parsed = lf.parse(&l.bytes);
        if f.matches(&l.bytes, &parsed, lf.names(), 0) {
            return Ok(Some(0));
        }
    }
    search::find_next(lf, f, 0, false, &|| work.passed())
}

/// Every filter the view of `of` applies: enabled chips, hidden levels and the time range.
fn view_filter_of(of: &OpenFile) -> Filter {
    let mut f = Filter::All;
    for chip in of.filters.iter().filter(|c| c.enabled) {
        if let Ok(x) = Filter::parse(&chip.text) {
            f = f.and(x);
        }
    }
    for l in &of.hidden_levels {
        f = f.and(Filter::Cmp { field: Field::Level, op: CmpOp::Ne, value: Literal::Level(*l) });
    }
    if let Some(t) = of.time_from {
        f = f.and(Filter::Cmp { field: Field::Time, op: CmpOp::Ge, value: Literal::Time(t) });
    }
    if let Some(t) = of.time_to {
        f = f.and(Filter::Cmp { field: Field::Time, op: CmpOp::Le, value: Literal::Time(t) });
    }
    f
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

fn show_view(s: &mut S, file: FileId, line: LineNo) {
    s.set_view(view_keys::ACTIVE_FILE, &file.0);
    s.set_view(view_keys::SELECTED_LINE, &line);
    s.set_view(view_keys::SCROLL_TO, &line);
}

// ---------------------------------------------------------------------------------------------
// Read-only commands
// ---------------------------------------------------------------------------------------------

/// log.open: open a file (`OpenFile` + `Live`), start the index job; result `results::Open`.
///
/// Without a window the file is indexed inline for the time budget (and further by every later
/// call); in a window only the first [`mcp::OPEN_WINDOWED_BYTES`] are, and the view's index job
/// does the rest. Opening a path that is already open shows that file again.
pub fn log_open(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_OPEN;
    let raw = str_param(p, "path").map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return Err(fail(id, "path is empty"));
    }
    let mut path = PathBuf::from(raw);
    if path.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        path = cwd.join(path);
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let follow = bool_param(p, "follow");
    let existing = s.doc().files.iter().find(|f| f.path == path).cloned();
    let (fid, lf) = match existing {
        Some(of) => {
            let lf = live(s, &of).map_err(|e| core(id, e))?;
            if follow && !of.follow {
                s.edit(labels::FOLLOW, |d| {
                    if let Some(f) = d.file_mut(of.id) {
                        f.follow = true;
                    }
                });
            }
            (of.id, lf)
        }
        None => {
            let fid = FileId(s.doc().next_id.max(1));
            let lf = Arc::new(open_log(fid, &path).map_err(|e| core(id, e))?);
            if s.is_windowed() {
                let _ = lf.index_more(mcp::OPEN_WINDOWED_BYTES);
                mcp::summarize(&lf, Deadline::after(mcp::budget(s)));
            } else {
                mcp::catch_up(&lf, follow, Deadline::after(mcp::budget(s)));
            }
            s.doc().live.insert(Arc::clone(&lf));
            let file = OpenFile { id: fid, path: path.clone(), follow, columns: default_columns(), redact: true, ..OpenFile::default() };
            s.edit(labels::OPEN_LOG, |d| {
                d.next_id = d.next_id.max(fid.0.saturating_add(1));
                d.files.push(file);
            });
            (fid, lf)
        }
    };
    s.set_view(view_keys::ACTIVE_FILE, &fid.0);
    let of = s.doc().file(fid).cloned().unwrap_or_default();
    let result = Open {
        file: fid.0,
        path: path.display().to_string(),
        bytes: lf.len(),
        format: of.format.clone().unwrap_or_else(|| lf.format().as_str().to_string()),
        lines: lf.lines(),
        following: of.follow,
        indexing: mcp::indexing_out(&lf),
    };
    let row = Row {
        summary: format!("Opened {} · {} · {}", file_name(&path), plural(lf.lines(), "line", "lines"), lf.format().label()),
        file: Some(fid.0),
        ..Row::default()
    };
    done(s, id, true, true, row, &result)
}

/// log.files: `results::Files`.
pub fn log_files(s: &mut S, p: &Value) -> R {
    let _ = p;
    let active = active(s);
    let doc_files = s.doc().files.clone();
    let mut files = Vec::new();
    for of in doc_files.iter().take(LIST_MAX) {
        let is_active = Some(of.id) == active;
        files.push(match live(s, of) {
            Ok(lf) => FileInfo {
                file: of.id.0,
                path: of.path.display().to_string(),
                format: of.format.clone().unwrap_or_else(|| lf.format().as_str().to_string()),
                bytes: lf.len(),
                lines: lf.lines(),
                time_span: mcp::time_span(&lf),
                following: of.follow,
                active: is_active,
                indexing: mcp::indexing_out(&lf),
            },
            Err(e) => FileInfo {
                file: of.id.0,
                path: of.path.display().to_string(),
                format: of.format.clone().unwrap_or_else(|| "plain".into()),
                bytes: 0,
                lines: 0,
                time_span: None,
                following: of.follow,
                active: is_active,
                indexing: Indexing { state: "failed".into(), done_bytes: 0, total_bytes: 0, error: Some(e.to_string()) },
            },
        });
    }
    let result = Files { omitted: (doc_files.len() - files.len()) as u64, files };
    let row = Row { summary: format!("Listed {}", plural(doc_files.len() as u64, "open file", "open files")), ..Row::default() };
    done(s, ids::LOG_FILES, true, false, row, &result)
}

/// log.stats: `results::Stats`.
pub fn log_stats(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_STATS;
    let t = target(s, p, id)?;
    let (mut from, mut to) = (None, None);
    if let Some(range) = p.get("range").filter(|r| !r.is_null()) {
        let obj = range.as_object().ok_or_else(|| fail(id, r#"range must be an object: {"from"?: time, "to"?: time}"#))?;
        let at = |k: &str| -> Result<Option<Timestamp>, CommandError> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(v)) => time::parse_time(v).map(Some).map_err(|e| core(id, e)),
                Some(_) => Err(fail(id, format!("range.{k} must be a time (RFC 3339)"))),
            }
        };
        from = at("from")?;
        to = at("to")?;
    }
    let st = match stats::stats(&t.lf, from, to, STATS_TOP_FIELDS, STATS_TOP_VALUES, &|| t.work.passed()) {
        Ok(st) => st,
        Err(Error::Cancelled) => return Err(fail(id, "timed out computing statistics; try a time range")),
        Err(e) => return Err(core(id, e)),
    };
    let mut o = out(s, &t);
    let top_fields = st
        .fields
        .iter()
        .take(STATS_TOP_FIELDS)
        .map(|f| TopField {
            name: f.name.clone(),
            lines: f.lines,
            values: f.top.iter().take(STATS_TOP_VALUES).map(|(v, c)| ValueCount { value: o.field(&f.name, v), count: *c }).collect(),
        })
        .collect();
    let time_span = match (st.first_time, st.last_time) {
        (Some(a), Some(b)) => Some(crate::results::TimeSpan { from: a.to_rfc3339(), to: b.to_rfc3339() }),
        _ => None,
    };
    let result = Stats {
        file: t.id(),
        lines: st.lines,
        bytes: st.bytes,
        time_span,
        levels: mcp::levels_out(&st.levels),
        top_fields,
        exact: st.exact && mcp::is_complete(&t.lf),
    };
    let row = Row {
        summary: format!("Read statistics · {} · {}", plural(st.lines, "line", "lines"), plural(st.levels.get(Level::Error), "error", "errors")),
        file: Some(t.id()),
        redacted: o.redacted,
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.search: `results::Search` (limit 50, max 500; redacted, cut to 2 KB).
pub fn log_search(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_SEARCH;
    let text = str_param(p, "query").unwrap_or_default();
    if text.is_empty() {
        return Err(fail(id, "query is empty"));
    }
    let query = Query {
        text: text.to_string(),
        regex: bool_param(p, "regex"),
        case_sensitive: bool_param(p, "case"),
        whole_word: bool_param(p, "whole_word"),
    };
    let matcher = query.compile().map_err(|e| core(id, e))?;
    let filter = filter_param(p, id)?;
    let limit = u64_param(p, "limit").unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX) as usize;
    let t = target(s, p, id)?;
    let mut found: Vec<LineNo> = Vec::new();
    let r = search::search(&t.lf, &matcher, filter.as_ref(), None, true, &|| t.work.passed(), &mut |m| {
        if found.len() < limit {
            found.push(m.line);
        }
        found.len() < limit
    });
    let (total, complete) = match r {
        Ok(sum) => (sum.matched.max(found.len() as u64), sum.complete && mcp::is_complete(&t.lf)),
        Err(Error::Cancelled) => (found.len() as u64, false),
        Err(e) => return Err(core(id, e)),
    };
    let mut o = out(s, &t);
    let matches: Vec<_> = found.iter().filter_map(|n| t.lf.line(*n, MAX_LINE_BYTES)).map(|l| o.hit(&t.lf, &l)).collect();
    let result = Search { file: t.id(), omitted: total.saturating_sub(matches.len() as u64), total, complete, matches };
    let more = if complete { "" } else { " so far" };
    let row = Row {
        summary: format!("Searched `{text}` · {}{more}", plural(total, "match", "matches")),
        file: Some(t.id()),
        line: result.matches.first().map(|h| h.line),
        redacted: o.redacted,
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.lines: `results::Lines`.
pub fn log_lines(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_LINES;
    let from = u64_param(p, "from").and_then(line_in).ok_or_else(|| fail(id, "from must be a line number (1-based)"))?;
    let count = u64_param(p, "count").unwrap_or(1).clamp(1, LINES_MAX);
    let t = target(s, p, id)?;
    let mut o = out(s, &t);
    let lines: Vec<_> = t.lf.lines_from(from, count as usize, MAX_LINE_BYTES).iter().map(|l| o.line(&t.lf, l)).collect();
    let result = Lines { file: t.id(), total_lines: t.lf.lines(), omitted: count.saturating_sub(lines.len() as u64), lines };
    let row = Row {
        summary: format!("Read {} from line {}", plural(result.lines.len() as u64, "line", "lines"), mcp::count(line_out(from))),
        file: Some(t.id()),
        line: Some(line_out(from)),
        redacted: o.redacted,
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.around: `results::Around`.
pub fn log_around(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_AROUND;
    let before = u64_param(p, "before").unwrap_or(AROUND_DEFAULT).min(AROUND_MAX);
    let after = u64_param(p, "after").unwrap_or(AROUND_DEFAULT).min(AROUND_MAX);
    let t = target(s, p, id)?;
    let at = line_param(&t, u64_param(p, "line").unwrap_or(0), id)?;
    let start = at.saturating_sub(before);
    let n = (at - start).saturating_add(1).saturating_add(after);
    let mut o = out(s, &t);
    let lines: Vec<_> = t.lf.lines_from(start, n as usize, MAX_LINE_BYTES).iter().map(|l| o.line(&t.lf, l)).collect();
    let result = crate::results::Around { file: t.id(), line: line_out(at), lines };
    let row = Row {
        summary: format!("Read {} around line {}", plural(result.lines.len() as u64, "line", "lines"), mcp::count(line_out(at))),
        file: Some(t.id()),
        line: Some(line_out(at)),
        redacted: o.redacted,
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.histogram: `results::HistogramOut`.
pub fn log_histogram(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_HISTOGRAM;
    let width = match str_param(p, "bucket").map(str::trim) {
        None | Some("") | Some("auto") => None,
        Some(d) => match time::parse_duration(d) {
            Ok(w) if w > 0 => Some(w),
            Ok(_) => return Err(fail(id, "bucket must be a positive duration like 30s, 5m or 1h")),
            Err(e) => return Err(core(id, e)),
        },
    };
    let filter = filter_param(p, id)?;
    let t = target(s, p, id)?;
    let h = match histogram::histogram(&t.lf, width, filter.as_ref(), &|| t.work.passed()) {
        Ok(h) => h,
        Err(Error::Cancelled) => return Err(fail(id, "timed out building the histogram; try a filter or a wider bucket")),
        Err(e) => return Err(core(id, e)),
    };
    let buckets: Vec<_> =
        h.buckets.iter().take(HISTOGRAM_MAX).map(|b| BucketOut { start: b.start.to_rfc3339(), levels: mcp::levels_out(&b.counts) }).collect();
    let result = HistogramOut {
        file: t.id(),
        bucket: time::format_duration(h.width),
        omitted: h.buckets.len().saturating_sub(buckets.len()) as u64,
        buckets,
        untimed: h.untimed,
    };
    let row = Row {
        summary: format!("Histogram · {} of {}", plural(result.buckets.len() as u64, "bucket", "buckets"), result.bucket),
        file: Some(t.id()),
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.fields: `results::Fields`.
pub fn log_fields(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_FIELDS;
    let t = target(s, p, id)?;
    let all = match stats::fields(&t.lf, FIELD_VALUES, &|| t.work.passed()) {
        Ok(f) => f,
        Err(Error::Cancelled) => return Err(fail(id, "timed out reading the fields")),
        Err(e) => return Err(core(id, e)),
    };
    let mut o = out(s, &t);
    let fields: Vec<_> = all
        .iter()
        .take(FIELDS_MAX)
        .map(|f| FieldOut {
            name: f.name.clone(),
            ty: f.ty.as_str().to_string(),
            lines: f.lines,
            distinct: f.distinct,
            values: f.top.iter().take(FIELD_VALUES).map(|(v, c)| ValueCount { value: o.field(&f.name, v), count: *c }).collect(),
        })
        .collect();
    let result = Fields { file: t.id(), omitted: all.len().saturating_sub(fields.len()) as u64, fields };
    let row = Row {
        summary: format!("Listed {}", plural(all.len() as u64, "field", "fields")),
        file: Some(t.id()),
        redacted: o.redacted,
        ..Row::default()
    };
    done(s, id, true, false, row, &result)
}

/// log.first: `results::First`.
pub fn log_first(s: &mut S, p: &Value) -> R {
    let id = ids::LOG_FIRST;
    let filter = filter_param(p, id)?.ok_or_else(|| fail(id, "filter is empty"))?;
    let t = target(s, p, id)?;
    let (found, timed_out) = match first_match(&t.lf, &filter, t.work) {
        Ok(f) => (f, false),
        Err(Error::Cancelled) => (None, true),
        Err(e) => return Err(core(id, e)),
    };
    let mut o = out(s, &t);
    let line = found.and_then(|n| t.lf.line(n, MAX_LINE_BYTES)).map(|l| o.line(&t.lf, &l));
    let text = filter.to_text();
    let result = First { file: t.id(), filter: text.clone(), complete: found.is_some() || (!timed_out && mcp::is_complete(&t.lf)), line };
    let summary = match &result.line {
        Some(l) => format!("First `{text}` · line {}", mcp::count(l.line)),
        None => format!("First `{text}` · no match"),
    };
    let row = Row { summary, file: Some(t.id()), line: result.line.as_ref().map(|l| l.line), redacted: o.redacted, ..Row::default() };
    done(s, id, true, false, row, &result)
}

/// bookmark.list: `results::Bookmarks`.
pub fn bookmark_list(s: &mut S, p: &Value) -> R {
    let id = ids::BOOKMARK_LIST;
    let t = target(s, p, id)?;
    let bookmarks: Vec<_> =
        t.of.bookmarks
            .iter()
            .take(LIST_MAX)
            .map(|b| BookmarkOut {
                bookmark: b.id,
                line: line_out(b.line),
                name: b.name.clone(),
                note: b.note.clone(),
                color: b.color.as_str().to_string(),
                by: match b.by {
                    Actor::User => "user".to_string(),
                    Actor::Agent => "agent".to_string(),
                },
            })
            .collect();
    let result = Bookmarks { file: t.id(), omitted: t.of.bookmarks.len().saturating_sub(bookmarks.len()) as u64, bookmarks };
    let row =
        Row { summary: format!("Listed {}", plural(t.of.bookmarks.len() as u64, "bookmark", "bookmarks")), file: Some(t.id()), ..Row::default() };
    done(s, id, true, false, row, &result)
}

// ---------------------------------------------------------------------------------------------
// Commands that change the view
// ---------------------------------------------------------------------------------------------

/// view.filter: add a `FilterChip` (label `ADD_FILTER`); `results::Filtered`.
pub fn view_filter(s: &mut S, p: &Value) -> R {
    let id = ids::VIEW_FILTER;
    check_allowed(s, id, true)?;
    let filter = filter_param(p, id)?.ok_or_else(|| fail(id, "filter is empty"))?;
    let t = target(s, p, id)?;
    let text = filter.to_text();
    let fid = t.of.id;
    let chip_id = s.edit(labels::ADD_FILTER, |d| {
        let chip = d.take_id();
        if let Some(f) = d.file_mut(fid) {
            f.filters.push(FilterChip { id: chip, text: text.clone(), enabled: true, by: Actor::Agent });
        }
        d.agent.pending.push(AgentChange::Filter { file: fid, id: chip });
        chip
    });
    let of = s.doc().file(fid).cloned().unwrap_or_default();
    let (visible, complete) = match search::count(&t.lf, &view_filter_of(&of), &|| t.work.passed()) {
        Ok(n) => (n, mcp::is_complete(&t.lf)),
        Err(Error::Cancelled) => (0, false),
        Err(e) => return Err(core(id, e)),
    };
    let result = Filtered { file: t.id(), filter_id: chip_id, filter: text.clone(), visible, complete };
    let row = Row {
        summary: format!("Filtered {text} · {} visible", plural(visible, "line", "lines")),
        file: Some(t.id()),
        item: Some(chip_id),
        ..Row::default()
    };
    done(s, id, true, true, row, &result)
}

/// view.clear_filters: label `CLEAR_FILTERS`; `results::Cleared`.
///
/// `agent.undo` does not bring cleared filters back (only Undo does): it removes what the agent
/// added.
pub fn view_clear_filters(s: &mut S, p: &Value) -> R {
    let id = ids::VIEW_CLEAR_FILTERS;
    let agent = mcp::by_agent(s, p, true);
    check_allowed(s, id, agent)?;
    let fid = match u64_param(p, "file") {
        Some(f) => FileId(f),
        None => active(s).ok_or_else(|| fail(id, NO_FILE))?,
    };
    if s.doc().file(fid).is_none() {
        return Err(fail(id, format!("no open file has id {} (log.files lists them)", fid.0)));
    }
    let removed = s.edit(labels::CLEAR_FILTERS, |d| {
        let n = d.file_mut(fid).map(|f| std::mem::take(&mut f.filters).len()).unwrap_or(0);
        d.agent.pending.retain(|c| !matches!(c, AgentChange::Filter { file, .. } if *file == fid));
        if !agent {
            person_acted(d);
        }
        n as u64
    });
    let result = Cleared { file: fid.0, removed };
    let row = Row { summary: format!("Cleared {}", plural(removed, "filter", "filters")), file: Some(fid.0), ..Row::default() };
    done(s, id, agent, true, row, &result)
}

/// view.goto: sets `view_keys::SCROLL_TO` and `SELECTED_LINE` (no undo step); `results::Goto`.
pub fn view_goto(s: &mut S, p: &Value) -> R {
    let id = ids::VIEW_GOTO;
    check_allowed(s, id, true)?;
    let t = target(s, p, id)?;
    let line = match (u64_param(p, "line"), str_param(p, "time")) {
        (Some(_), Some(_)) => return Err(fail(id, "give line or time, not both")),
        (Some(n), None) => line_param(&t, n, id)?,
        (None, Some(time_text)) => {
            let at = time::parse_time(time_text).map_err(|e| core(id, e))?;
            let f = Filter::Cmp { field: Field::Time, op: CmpOp::Ge, value: Literal::Time(at) };
            match first_match(&t.lf, &f, t.work) {
                Ok(Some(n)) => n,
                Ok(None) => return Err(fail(id, format!("no line at or after {}", at.to_rfc3339()))),
                Err(Error::Cancelled) => return Err(fail(id, format!("timed out looking for {}", at.to_rfc3339()))),
                Err(e) => return Err(core(id, e)),
            }
        }
        (None, None) => return Err(fail(id, "give line or time")),
    };
    show_view(s, t.of.id, line);
    let result = Goto { file: t.id(), line: line_out(line) };
    let row =
        Row { summary: format!("Went to line {}", mcp::count(line_out(line))), file: Some(t.id()), line: Some(line_out(line)), ..Row::default() };
    done(s, id, true, true, row, &result)
}

/// view.highlight: label `HIGHLIGHT`; `results::Highlighted`.
pub fn view_highlight(s: &mut S, p: &Value) -> R {
    let id = ids::VIEW_HIGHLIGHT;
    check_allowed(s, id, true)?;
    let t = target(s, p, id)?;
    let note = str_param(p, "note").map(str::to_string).filter(|n| !n.trim().is_empty());
    let given = p.get("lines").filter(|v| !v.is_null());
    let query = str_param(p, "query").filter(|q| !q.is_empty());
    let (mut lines, total, by_query): (Vec<LineNo>, u64, Option<String>) = match (given, query) {
        (Some(_), Some(_)) => return Err(fail(id, "give lines or query, not both")),
        (None, None) => return Err(fail(id, "give lines (an array of line numbers) or query")),
        (Some(v), None) => {
            let arr = v.as_array().ok_or_else(|| fail(id, "lines must be an array of line numbers (1-based)"))?;
            let mut out = Vec::with_capacity(arr.len().min(HIGHLIGHT_MAX as usize));
            for x in arr {
                let n = x.as_u64().ok_or_else(|| fail(id, format!("lines: {x} is not a line number (1-based)")))?;
                out.push(line_param(&t, n, id)?);
            }
            out.sort_unstable();
            out.dedup();
            let total = out.len() as u64;
            (out, total, None)
        }
        (None, Some(q)) => {
            let matcher = Query::text(q).compile().map_err(|e| core(id, e))?;
            let mut out = Vec::new();
            let cap = HIGHLIGHT_MAX as usize;
            let r = search::search(&t.lf, &matcher, None, None, true, &|| t.work.passed(), &mut |m| {
                if out.len() < cap {
                    out.push(m.line);
                }
                out.len() < cap
            });
            let total = match r {
                Ok(sum) => sum.matched.max(out.len() as u64),
                Err(Error::Cancelled) => out.len() as u64,
                Err(e) => return Err(core(id, e)),
            };
            if out.is_empty() {
                return Err(fail(id, format!("no line matches `{q}`")));
            }
            (out, total, Some(q.to_string()))
        }
    };
    lines.truncate(HIGHLIGHT_MAX as usize);
    let n = lines.len() as u64;
    let first = lines.first().copied();
    let fid = t.of.id;
    let hl = s.edit(labels::HIGHLIGHT, |d| {
        let hl = d.take_id();
        if let Some(f) = d.file_mut(fid) {
            f.highlights.push(Highlight { id: hl, lines, query: by_query, note, by: Actor::Agent });
        }
        d.agent.pending.push(AgentChange::Highlight { file: fid, id: hl });
        hl
    });
    if let Some(l) = first {
        show_view(s, fid, l);
    }
    let result = Highlighted { file: t.id(), highlight: hl, lines: n, omitted: total.saturating_sub(n) };
    let row = Row {
        summary: format!("Highlighted {}", plural(n, "line", "lines")),
        file: Some(t.id()),
        line: first.map(line_out),
        item: Some(hl),
        ..Row::default()
    };
    done(s, id, true, true, row, &result)
}

/// bookmark.add: label `ADD_BOOKMARK`; `results::BookmarkAdded`.
pub fn bookmark_add(s: &mut S, p: &Value) -> R {
    let id = ids::BOOKMARK_ADD;
    let agent = mcp::by_agent(s, p, true);
    check_allowed(s, id, agent)?;
    let t = target(s, p, id)?;
    let line = match u64_param(p, "line") {
        Some(n) => line_param(&t, n, id)?,
        None => {
            let shown = s.view::<u64>(view_keys::ACTIVE_FILE).is_none_or(|a| a == t.id());
            match s.view::<u64>(view_keys::SELECTED_LINE).filter(|_| shown) {
                Some(l) if l < t.lf.lines() => l,
                _ => return Err(fail(id, "no line given and no line is selected")),
            }
        }
    };
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| format!("Line {}", line_out(line)), str::to_string);
    let note = str_param(p, "note").map(str::to_string).filter(|n| !n.trim().is_empty());
    let fid = t.of.id;
    let bm_name = name.clone();
    let bm = s.edit(labels::ADD_BOOKMARK, |d| {
        let bm = d.take_id();
        if let Some(f) = d.file_mut(fid) {
            f.bookmarks.push(Bookmark { id: bm, line, name: bm_name, note, color: BookmarkColor::Amber, by: actor(agent) });
        }
        if agent {
            d.agent.pending.push(AgentChange::Bookmark { file: fid, id: bm });
        } else {
            person_acted(d);
        }
        bm
    });
    let result = BookmarkAdded { file: t.id(), bookmark: bm, line: line_out(line), name: name.clone() };
    let row = Row { summary: format!("Added bookmark '{name}'"), file: Some(t.id()), line: Some(line_out(line)), item: Some(bm), ..Row::default() };
    done(s, id, agent, true, row, &result)
}

/// agent.note: label `AGENT_NOTE`; `results::Noted`.
pub fn agent_note(s: &mut S, p: &Value) -> R {
    let id = ids::AGENT_NOTE;
    check_allowed(s, id, true)?;
    let text = str_param(p, "text").map(str::trim).unwrap_or_default().to_string();
    if text.is_empty() {
        return Err(fail(id, "text is empty"));
    }
    if text.len() > NOTE_MAX_BYTES {
        return Err(fail(id, format!("the note is {} bytes; the most is {NOTE_MAX_BYTES}", text.len())));
    }
    let summary = strata_core::limits::cut(&text, 80);
    let summary = format!("Note: {}{}", summary.0, if summary.1 { "…" } else { "" });
    let note = s.edit(labels::AGENT_NOTE, |d| {
        let n = d.take_id();
        d.agent.notes.push(AgentNote { id: n, text });
        d.agent.pending.push(AgentChange::Note { id: n });
        n
    });
    done(s, id, true, true, Row { summary, item: Some(note), ..Row::default() }, &Noted { note })
}

/// agent.undo: one step labelled `UNDO_AGENT_CHANGES`; `results::Undone`.
///
/// Removes exactly the filters, highlights, bookmarks and notes the agent added since the person
/// last acted; Undo (Cmd+Z) after it brings them back.
pub fn agent_undo(s: &mut S, p: &Value) -> R {
    let agent = mcp::by_agent(s, p, true);
    let pending = s.doc().agent.pending.clone();
    let undone = s.edit(labels::UNDO_AGENT_CHANGES, |d| {
        let mut n = 0u64;
        for change in &pending {
            let gone = match change {
                AgentChange::Filter { file, id } => d.file_mut(*file).map(|f| remove(&mut f.filters, |c| c.id == *id)),
                AgentChange::Highlight { file, id } => d.file_mut(*file).map(|f| remove(&mut f.highlights, |h| h.id == *id)),
                AgentChange::Bookmark { file, id } => d.file_mut(*file).map(|f| remove(&mut f.bookmarks, |b| b.id == *id)),
                AgentChange::Note { id } => Some(remove(&mut d.agent.notes, |n| n.id == *id)),
            };
            n = n.saturating_add(gone.unwrap_or(0));
        }
        d.agent.pending.clear();
        n
    });
    let row = Row { summary: format!("Undid {}", plural(undone, "agent change", "agent changes")), ..Row::default() };
    done(s, ids::AGENT_UNDO, agent, true, row, &Undone { undone })
}

/// Remove the items `gone` picks; how many.
fn remove<T>(v: &mut Vec<T>, gone: impl Fn(&T) -> bool) -> u64 {
    let before = v.len();
    v.retain(|x| !gone(x));
    (before - v.len()) as u64
}
