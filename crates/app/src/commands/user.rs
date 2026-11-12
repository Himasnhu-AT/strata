//! Handlers of the user commands (menus, palette, keyboard). Owned by the `ui` stream: fill in the
//! bodies and predicates, keep the signatures (registered in `commands/mod.rs`).
//!
//! A handler changes the document with one `edit(s, labels::…)` (one undo step), or view state
//! (selection, panels, the search: not undoable). Work a command cannot do on the session (start
//! a job, show a dialog, use the clipboard) is left for the window as a
//! [`Request`](crate::views::req::Request); without a window (CLI, MCP) the commands that can
//! answer at once do so inline.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ashlar::prelude::*;
use strata_core::filter::{CmpOp, Field, Literal};
use strata_core::{FileId, Filter, Level, LogFile, Timestamp};

use super::{R, S, ids};
use crate::doc::{Actor, Bookmark, BookmarkColor, Column, FilterChip, OpenFile, SavedFilter, default_columns, labels, view_keys};
use crate::views::req::{self, Request, Target};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// The file shown in the window: `view_keys::ACTIVE_FILE` when it is open, else the first file.
pub fn active_file(s: &S) -> Option<FileId> {
    let files = &s.doc().files;
    let want = s.view::<u64>(view_keys::ACTIVE_FILE).map(FileId);
    want.filter(|id| files.iter().any(|f| f.id == *id)).or_else(|| files.first().map(|f| f.id))
}

/// The `file` parameter, or the active file; fails when it is not open.
fn file_arg(s: &S, p: &Value, id: &str) -> Result<FileId, CommandError> {
    match p.get("file").and_then(Value::as_u64) {
        Some(n) if s.doc().file(FileId(n)).is_some() => Ok(FileId(n)),
        Some(n) => Err(CommandError::failed(id, format!("no open file has id {n}"))),
        None => active_file(s).ok_or_else(|| CommandError::failed(id, "no log file is open")),
    }
}

fn str_arg<'a>(p: &'a Value, name: &str) -> Option<&'a str> {
    p.get(name).and_then(Value::as_str)
}

fn bool_arg(p: &Value, name: &str) -> Option<bool> {
    p.get(name).and_then(Value::as_bool)
}

fn u64_arg(p: &Value, name: &str) -> Option<u64> {
    p.get(name).and_then(Value::as_u64)
}

fn required<'a>(p: &'a Value, name: &str, id: &str) -> Result<&'a str, CommandError> {
    str_arg(p, name).ok_or_else(|| CommandError::bad_params(id, format!("`{name}` is required")))
}

/// The selected line (0-based) when it is in `file`.
fn selected_line(s: &S, file: FileId) -> Option<u64> {
    (active_file(s) == Some(file)).then(|| s.view::<u64>(view_keys::SELECTED_LINE)).flatten()
}

/// Select line `line` (0-based) of `file` and scroll to it.
pub fn select(s: &mut S, file: FileId, line: u64) {
    s.set_view(view_keys::ACTIVE_FILE, &file.0);
    s.set_view(view_keys::SELECTED_LINE, &line);
    s.set_view(view_keys::SCROLL_TO, &line);
}

/// A string as the filter grammar writes it: bare when it can be, else quoted.
pub fn quote(value: &str) -> String {
    let bare = !value.is_empty()
        && !matches!(value, "and" | "or" | "not")
        && !value.starts_with('-')
        && !value.starts_with('/')
        && value.chars().all(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '"' | '=' | '!' | '<' | '>' | '~' | '\\'));
    if bare {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The filter that finds lines of `target` (`level >= error`, `level = warn`, `time >= t`).
pub fn target_filter(target: Target, time: Option<i64>) -> Filter {
    match target {
        Target::Error => Filter::Cmp { field: Field::Level, op: CmpOp::Ge, value: Literal::Level(Level::Error) },
        Target::Warning => Filter::Cmp { field: Field::Level, op: CmpOp::Eq, value: Literal::Level(Level::Warn) },
        Target::Time => Filter::Cmp { field: Field::Time, op: CmpOp::Ge, value: Literal::Time(Timestamp(time.unwrap_or(0))) },
    }
}

/// Whether `path` starts like a gzip stream.
fn is_gzip_file(path: &Path) -> bool {
    let mut head = [0u8; 2];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && strata_core::gz::is_gzip(&head)
}

/// Open the log at `path` as a new file (or switch to it when it is open already): one undo step
/// `Open Log`. The file is mapped and its format detected now (cheap); indexing runs as a job the
/// window starts. A `.gz` file is decompressed by that job first. Fails for a missing,
/// unreadable or binary file, with the reason. The `agent` stream's `log.open` can call this.
pub fn open_path(s: &mut S, path: &Path, follow: bool) -> Result<FileId, CommandError> {
    // the same spelling `log.open` uses, so a file is open once whichever way it came
    let path: PathBuf = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if let Some(f) = s.doc().files.iter().find(|f| f.path == path) {
        let id = f.id;
        s.set_view(view_keys::ACTIVE_FILE, &id.0);
        return Ok(id);
    }
    // `take_id` inside the edit hands out exactly this id
    let id = FileId(s.doc().next_id.max(1));
    if !is_gzip_file(&path) {
        let file = LogFile::open(id, &path).map_err(|e| CommandError::failed(ids::LOG_OPEN_DIALOG, e.to_string()))?;
        s.doc().live.insert(Arc::new(file));
    } else if !path.is_file() {
        return Err(CommandError::failed(ids::LOG_OPEN_DIALOG, format!("not found: {}", path.display())));
    }
    let id = edit(s, labels::OPEN_LOG, |d| {
        let id = FileId(d.take_id());
        d.files.push(OpenFile { id, path: path.clone(), follow, columns: default_columns(), redact: true, ..OpenFile::default() });
        id
    });
    s.set_view(view_keys::ACTIVE_FILE, &id.0);
    s.set_view_value(view_keys::SELECTED_LINE, Value::Null);
    Ok(id)
}

/// Run a line search now (no window to run it as a job): select the line found.
fn find_inline(s: &mut S, file: FileId, target: Target, back: bool, from: Option<u64>, time: Option<i64>) -> R {
    let Some(log) = s.doc().live.get(file) else { return Ok(json!({"line": null})) };
    let filter = target_filter(target, time);
    let start = match (from, target) {
        (_, Target::Time) => 0,
        (Some(l), _) => l,
        (None, _) if back => log.lines(),
        (None, _) => 0,
    };
    let found = strata_core::search::find_next(&log, &filter, start, back, &|| false).map_err(|e| CommandError::failed("find", e.to_string()))?;
    if let Some(line) = found {
        select(s, file, line);
    }
    Ok(json!({"line": found.map(|l| l.saturating_add(1))}))
}

fn find(s: &mut S, p: &Value, id: &str, target: Target, back: bool) -> R {
    let file = file_arg(s, p, id)?;
    let from = selected_line(s, file);
    if !s.is_windowed() {
        return find_inline(s, file, target, back, from, None);
    }
    req::push(s, Request::Find { file: file.0, target, back, from, time: None });
    Ok(json!({"requested": true}))
}

fn chip_text_for_save(f: &OpenFile) -> String {
    let on: Vec<&FilterChip> = f.filters.iter().filter(|c| c.enabled && !c.text.trim().is_empty()).collect();
    match on.as_slice() {
        [] => String::new(),
        [one] => one.text.clone(),
        many => many.iter().map(|c| format!("({})", c.text)).collect::<Vec<_>>().join(" and "),
    }
}

/// The columns of a file as saved (the defaults when none are).
pub fn columns_of(f: &OpenFile) -> Vec<Column> {
    if f.columns.is_empty() { default_columns() } else { f.columns.clone() }
}

/// Every document change the person makes: one undo step that also tells the agent stream the
/// person acted (`agent.undo` then reverts only agent changes made after it).
fn edit<T>(s: &mut S, label: &str, f: impl FnOnce(&mut crate::doc::StrataDoc) -> T) -> T {
    s.edit(label, |d| {
        super::agent::person_acted(d);
        f(d)
    })
}

// ---------------------------------------------------------------------------------------------
// Enabled predicates
// ---------------------------------------------------------------------------------------------

/// Enabled when a file is open.
pub fn has_file(s: &S) -> Result<(), String> {
    if s.doc().files.is_empty() { Err("No log file is open".into()) } else { Ok(()) }
}

/// Enabled while the active file is indexing.
pub fn is_indexing(s: &S) -> Result<(), String> {
    has_file(s)?;
    let live = active_file(s).and_then(|id| s.doc().live.get(id));
    match live.map(|f| f.index_state()) {
        Some(strata_core::IndexState::Running { .. } | strata_core::IndexState::Idle) => Ok(()),
        _ => Err("The file is not being indexed".into()),
    }
}

/// Enabled when the active file has a search.
pub fn has_search(s: &S) -> Result<(), String> {
    let query = s.view_state().get(view_keys::SEARCH).and_then(|v| v.get("query")).and_then(Value::as_str).unwrap_or_default();
    if query.is_empty() { Err("Nothing is being searched".into()) } else { Ok(()) }
}

/// Enabled when a line is selected.
pub fn has_selection(s: &S) -> Result<(), String> {
    match s.view::<u64>(view_keys::SELECTED_LINE) {
        Some(_) if !s.doc().files.is_empty() => Ok(()),
        _ => Err("No line is selected".into()),
    }
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// Ask for a log file (the view shows the native dialog, then opens it); with `path`, open that
/// file at once (drag and drop, recent files, scripts).
pub fn log_open_dialog(s: &mut S, p: &Value) -> R {
    let follow = bool_arg(p, "follow").unwrap_or(false);
    if let Some(path) = str_arg(p, "path") {
        let id = open_path(s, Path::new(path), follow)?;
        let path = s.doc().file(id).map(|f| f.path.display().to_string()).unwrap_or_default();
        return Ok(json!({"file": id.0, "path": path}));
    }
    if !s.is_windowed() {
        return Err(CommandError::bad_params(ids::LOG_OPEN_DIALOG, "there is no window to ask in: give `path`"));
    }
    req::push(s, Request::OpenDialog);
    Ok(json!({"requested": true}))
}

/// Close a file (label `CLOSE_LOG`).
pub fn log_close(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::LOG_CLOSE)?;
    let next = {
        let files = &s.doc().files;
        let at = files.iter().position(|f| f.id == file).unwrap_or(0);
        files.iter().filter(|f| f.id != file).nth(at.saturating_sub(1)).map(|f| f.id)
    };
    edit(s, labels::CLOSE_LOG, |d| d.files.retain(|f| f.id != file));
    s.doc().live.remove(file);
    if s.view::<u64>(view_keys::ACTIVE_FILE) == Some(file.0) || active_file(s).is_none() {
        match next {
            Some(n) => s.set_view(view_keys::ACTIVE_FILE, &n.0),
            None => s.set_view_value(view_keys::ACTIVE_FILE, Value::Null),
        }
        s.set_view_value(view_keys::SELECTED_LINE, Value::Null);
    }
    Ok(json!({"closed": file.0}))
}

/// Reopen a file from disk (after "The file was replaced. Reload?").
pub fn log_reload(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::LOG_RELOAD)?;
    s.doc().live.remove(file);
    s.set_view_value(req::keys::FILE_CHANGED, Value::Null);
    if !s.is_windowed() {
        let path = s.doc().file(file).map(|f| f.path.clone()).unwrap_or_default();
        let log = LogFile::open(file, &path).map_err(|e| CommandError::failed(ids::LOG_RELOAD, e.to_string()))?;
        log.index_all(8 << 20, &|| false, &mut |_, _| {}).map_err(|e| CommandError::failed(ids::LOG_RELOAD, e.to_string()))?;
        let lines = log.lines();
        s.doc().live.insert(Arc::new(log));
        return Ok(json!({"file": file.0, "lines": lines}));
    }
    Ok(json!({"file": file.0}))
}

/// Cancel the index job of a file (Esc in the status bar).
pub fn log_stop_indexing(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::LOG_STOP_INDEXING)?;
    req::push(s, Request::StopIndexing { file: file.0 });
    Ok(json!({"file": file.0}))
}

/// Override the detected format (label `SET_FORMAT`).
pub fn log_set_format(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::LOG_SET_FORMAT)?;
    let format = required(p, "format", ids::LOG_SET_FORMAT)?.to_string();
    if strata_core::FormatKind::from_id(&format).is_none() {
        return Err(CommandError::bad_params(ids::LOG_SET_FORMAT, format!("unknown format `{format}`")));
    }
    edit(s, labels::SET_FORMAT, |d| {
        if let Some(f) = d.file_mut(file) {
            f.format = Some(format.clone());
        }
    });
    Ok(json!({"file": file.0, "format": format}))
}

/// Turn redaction of agent results on or off for a file (label `REDACTION`).
pub fn log_redaction(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::LOG_REDACTION)?;
    let on = bool_arg(p, "on").ok_or_else(|| CommandError::bad_params(ids::LOG_REDACTION, "`on` is required"))?;
    edit(s, labels::REDACTION, |d| {
        if let Some(f) = d.file_mut(file) {
            f.redact = on;
        }
    });
    Ok(json!({"file": file.0, "redact": on}))
}

// ---------------------------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------------------------

/// Focus the search field (`view_keys::FOCUS` = `search`).
pub fn search_focus(s: &mut S, _p: &Value) -> R {
    s.set_view(view_keys::FOCUS, "search");
    Ok(json!({}))
}

/// Start a search job; matches stream into the match list (`view_keys::SEARCH`).
pub fn search_run(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::SEARCH_RUN)?;
    let query = required(p, "query", ids::SEARCH_RUN)?.to_string();
    let q = strata_core::Query {
        text: query.clone(),
        regex: bool_arg(p, "regex").unwrap_or(false),
        case_sensitive: bool_arg(p, "case").unwrap_or(false),
        whole_word: bool_arg(p, "whole_word").unwrap_or(false),
    };
    q.compile().map_err(|e| CommandError::failed(ids::SEARCH_RUN, e.to_string()))?;
    if query.is_empty() {
        s.set_view_value(view_keys::SEARCH, Value::Null);
        return Ok(json!({"query": ""}));
    }
    let search = json!({"file": file.0, "query": q.text, "regex": q.regex, "case": q.case_sensitive, "whole_word": q.whole_word});
    s.set_view(view_keys::ACTIVE_FILE, &file.0);
    s.set_view_value(view_keys::SEARCH, search.clone());
    Ok(search)
}

/// Select the next match.
pub fn search_next(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::SEARCH_NEXT)?;
    req::push(s, Request::StepMatch { file: file.0, back: false });
    Ok(json!({"requested": true}))
}

/// Select the previous match.
pub fn search_prev(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::SEARCH_PREV)?;
    req::push(s, Request::StepMatch { file: file.0, back: true });
    Ok(json!({"requested": true}))
}

/// Clear the search.
pub fn search_clear(s: &mut S, _p: &Value) -> R {
    s.set_view_value(view_keys::SEARCH, Value::Null);
    Ok(json!({}))
}

// ---------------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------------

/// Focus the filter bar.
pub fn filter_focus(s: &mut S, _p: &Value) -> R {
    s.set_view(view_keys::FOCUS, "filter");
    Ok(json!({}))
}

fn chip_edit(s: &mut S, p: &Value, id: &str, label: &str, f: impl FnOnce(&mut Vec<FilterChip>, usize)) -> R {
    let file = file_arg(s, p, id)?;
    let chip = u64_arg(p, "id").ok_or_else(|| CommandError::bad_params(id, "`id` is required"))?;
    let at = s.doc().file(file).and_then(|f| f.filters.iter().position(|c| c.id == chip));
    let Some(at) = at else { return Err(CommandError::failed(id, format!("no filter {chip}"))) };
    s.edit(label, |d| {
        if let Some(file) = d.file_mut(file)
            && at < file.filters.len()
        {
            f(&mut file.filters, at);
        }
    });
    Ok(json!({"file": file.0, "id": chip}))
}

/// Turn a filter chip on or off (label `TOGGLE_FILTER`).
pub fn filter_toggle(s: &mut S, p: &Value) -> R {
    chip_edit(s, p, ids::FILTER_TOGGLE, labels::TOGGLE_FILTER, |chips, at| {
        if let Some(c) = chips.get_mut(at) {
            c.enabled = !c.enabled;
        }
    })
}

/// Remove a filter chip (label `REMOVE_FILTER`).
pub fn filter_remove(s: &mut S, p: &Value) -> R {
    chip_edit(s, p, ids::FILTER_REMOVE, labels::REMOVE_FILTER, |chips, at| {
        chips.remove(at);
    })
}

/// Show or hide a level (label `TOGGLE_LEVEL`).
pub fn filter_level(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::FILTER_LEVEL)?;
    let word = required(p, "level", ids::FILTER_LEVEL)?;
    let level = Level::parse(word).ok_or_else(|| CommandError::bad_params(ids::FILTER_LEVEL, format!("unknown level `{word}`")))?;
    let hidden = s.doc().file(file).is_some_and(|f| f.hidden_levels.contains(&level));
    let show = bool_arg(p, "on").unwrap_or(hidden);
    edit(s, labels::TOGGLE_LEVEL, |d| {
        if let Some(f) = d.file_mut(file) {
            f.hidden_levels.retain(|l| *l != level);
            if !show {
                f.hidden_levels.push(level);
                f.hidden_levels.sort();
            }
        }
    });
    Ok(json!({"file": file.0, "level": level.as_str(), "shown": show}))
}

/// Limit to a time range (label `TIME_RANGE`).
pub fn filter_time_range(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::FILTER_TIME_RANGE)?;
    let parse = |name: &str| -> Result<Option<Timestamp>, CommandError> {
        match str_arg(p, name).filter(|t| !t.trim().is_empty()) {
            Some(t) => strata_core::time::parse_time(t).map(Some).map_err(|e| CommandError::bad_params(ids::FILTER_TIME_RANGE, e.to_string())),
            None => Ok(None),
        }
    };
    let (from, to) = (parse("from")?, parse("to")?);
    if let (Some(a), Some(b)) = (from, to)
        && a > b
    {
        return Err(CommandError::bad_params(ids::FILTER_TIME_RANGE, "`from` is after `to`"));
    }
    edit(s, labels::TIME_RANGE, |d| {
        if let Some(f) = d.file_mut(file) {
            f.time_from = from;
            f.time_to = to;
        }
    });
    Ok(json!({"file": file.0, "from": from.map(Timestamp::to_rfc3339), "to": to.map(Timestamp::to_rfc3339)}))
}

/// Save the active filters by name (label `SAVE_FILTER`).
pub fn filter_save(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::FILTER_SAVE)?;
    let name = required(p, "name", ids::FILTER_SAVE)?.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::bad_params(ids::FILTER_SAVE, "the name is empty"));
    }
    let text = s.doc().file(file).map(chip_text_for_save).unwrap_or_default();
    if text.is_empty() {
        return Err(CommandError::failed(ids::FILTER_SAVE, "no filter is on"));
    }
    edit(s, labels::SAVE_FILTER, |d| {
        d.saved_filters.retain(|f| f.name != name);
        d.saved_filters.push(SavedFilter { name: name.clone(), text: text.clone() });
    });
    Ok(json!({"name": name, "filter": text}))
}

/// Apply a saved filter (label `APPLY_SAVED_FILTER`).
pub fn filter_apply_saved(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::FILTER_APPLY_SAVED)?;
    let name = required(p, "name", ids::FILTER_APPLY_SAVED)?;
    let Some(saved) = s.doc().saved_filters.iter().find(|f| f.name == name).cloned() else {
        return Err(CommandError::failed(ids::FILTER_APPLY_SAVED, format!("no saved filter `{name}`")));
    };
    let chip = edit(s, labels::APPLY_SAVED_FILTER, |d| {
        let id = d.take_id();
        if let Some(f) = d.file_mut(file) {
            f.filters.push(FilterChip { id, text: saved.text.clone(), enabled: true, by: Actor::User });
        }
        id
    });
    Ok(json!({"file": file.0, "id": chip, "filter": saved.text}))
}

/// Delete a saved filter (label `DELETE_SAVED_FILTER`).
pub fn filter_delete_saved(s: &mut S, p: &Value) -> R {
    let name = required(p, "name", ids::FILTER_DELETE_SAVED)?.to_string();
    if !s.doc().saved_filters.iter().any(|f| f.name == name) {
        return Err(CommandError::failed(ids::FILTER_DELETE_SAVED, format!("no saved filter `{name}`")));
    }
    edit(s, labels::DELETE_SAVED_FILTER, |d| d.saved_filters.retain(|f| f.name != name));
    Ok(json!({"name": name}))
}

// ---------------------------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------------------------

/// Select the next error line.
pub fn nav_next_error(s: &mut S, p: &Value) -> R {
    find(s, p, ids::NAV_NEXT_ERROR, Target::Error, false)
}

/// Select the previous error line.
pub fn nav_prev_error(s: &mut S, p: &Value) -> R {
    find(s, p, ids::NAV_PREV_ERROR, Target::Error, true)
}

/// Select the next warning line.
pub fn nav_next_warning(s: &mut S, p: &Value) -> R {
    find(s, p, ids::NAV_NEXT_WARNING, Target::Warning, false)
}

/// Select the previous warning line.
pub fn nav_prev_warning(s: &mut S, p: &Value) -> R {
    find(s, p, ids::NAV_PREV_WARNING, Target::Warning, true)
}

/// Open the go-to-line-or-time field.
pub fn nav_goto_prompt(s: &mut S, _p: &Value) -> R {
    s.set_view(view_keys::FOCUS, "goto");
    Ok(json!({}))
}

/// Go to `text`: a 1-based line number, or a time (the first line at or after it). Used by the
/// go-to field.
pub fn goto_text(s: &mut S, file: FileId, text: &str) -> R {
    let t = text.trim().replace([',', '_'], "");
    if let Ok(n) = t.parse::<u64>() {
        let line = n.max(1) - 1;
        let last = s.doc().live.get(file).map(|f| f.lines().saturating_sub(1)).unwrap_or(0);
        select(s, file, line.min(last));
        return Ok(json!({"line": line.min(last) + 1}));
    }
    let time = strata_core::time::parse_time(text).map_err(|e| CommandError::failed(ids::NAV_GOTO_PROMPT, e.to_string()))?;
    if !s.is_windowed() {
        return find_inline(s, file, Target::Time, false, None, Some(time.0));
    }
    req::push(s, Request::Find { file: file.0, target: Target::Time, back: false, from: None, time: Some(time.0) });
    Ok(json!({"time": time.to_rfc3339()}))
}

// ---------------------------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------------------------

/// Follow a file or stop (label `FOLLOW`).
pub fn view_follow(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::VIEW_FOLLOW)?;
    let now = s.doc().file(file).is_some_and(|f| f.follow);
    let on = bool_arg(p, "on").unwrap_or(!now);
    if on != now {
        edit(s, labels::FOLLOW, |d| {
            if let Some(f) = d.file_mut(file) {
                f.follow = on;
            }
        });
    }
    Ok(json!({"file": file.0, "follow": on}))
}

/// Select a line (view state, no undo step).
pub fn view_select(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::VIEW_SELECT)?;
    let line = u64_arg(p, "line").filter(|l| *l >= 1).ok_or_else(|| CommandError::bad_params(ids::VIEW_SELECT, "`line` (1-based) is required"))?;
    select(s, file, line - 1);
    Ok(json!({"file": file.0, "line": line}))
}

/// Show or hide the detail pane.
pub fn view_toggle_detail(s: &mut S, _p: &Value) -> R {
    let open = !s.view_or(view_keys::DETAIL_OPEN, true);
    s.set_view(view_keys::DETAIL_OPEN, &open);
    Ok(json!({"open": open}))
}

/// Show or hide the agent panel.
pub fn view_toggle_agent_panel(s: &mut S, _p: &Value) -> R {
    let open = !s.view_or(view_keys::AGENT_PANEL_OPEN, false);
    s.set_view(view_keys::AGENT_PANEL_OPEN, &open);
    Ok(json!({"open": open}))
}

/// Show the Bookmarks section of the sidebar.
pub fn view_show_bookmarks(s: &mut S, _p: &Value) -> R {
    s.set_view(view_keys::FOCUS, "bookmarks");
    Ok(json!({}))
}

/// Show or hide a column (label `TOGGLE_COLUMN`). A field not yet a column is added before the
/// message.
pub fn view_toggle_column(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::VIEW_TOGGLE_COLUMN)?;
    let key = required(p, "column", ids::VIEW_TOGGLE_COLUMN)?.trim().to_string();
    if key.is_empty() {
        return Err(CommandError::bad_params(ids::VIEW_TOGGLE_COLUMN, "the column is empty"));
    }
    let cols = s.doc().file(file).map(columns_of).unwrap_or_default();
    let now = cols.iter().find(|c| c.key == key).map(|c| c.visible);
    let on = bool_arg(p, "on").unwrap_or(!now.unwrap_or(false));
    if !on && !cols.iter().any(|c| c.visible && c.key != key) {
        return Err(CommandError::failed(ids::VIEW_TOGGLE_COLUMN, "the table needs one column"));
    }
    edit(s, labels::TOGGLE_COLUMN, |d| {
        if let Some(f) = d.file_mut(file) {
            let mut cols = columns_of(f);
            match cols.iter_mut().find(|c| c.key == key) {
                Some(c) => c.visible = on,
                None => {
                    let at = cols.iter().position(|c| c.key == "message").unwrap_or(cols.len());
                    cols.insert(at, Column { key: key.clone(), visible: on, width: None });
                }
            }
            f.columns = cols;
        }
    });
    Ok(json!({"file": file.0, "column": key, "visible": on}))
}

/// Move a column (label `MOVE_COLUMN`).
pub fn view_move_column(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::VIEW_MOVE_COLUMN)?;
    let key = required(p, "column", ids::VIEW_MOVE_COLUMN)?.to_string();
    let to =
        usize::try_from(u64_arg(p, "to").ok_or_else(|| CommandError::bad_params(ids::VIEW_MOVE_COLUMN, "`to` is required"))?).unwrap_or(usize::MAX);
    let cols = s.doc().file(file).map(columns_of).unwrap_or_default();
    if !cols.iter().any(|c| c.key == key) {
        return Err(CommandError::failed(ids::VIEW_MOVE_COLUMN, format!("no column `{key}`")));
    }
    edit(s, labels::MOVE_COLUMN, |d| {
        if let Some(f) = d.file_mut(file) {
            let mut cols = columns_of(f);
            if let Some(at) = cols.iter().position(|c| c.key == key) {
                let c = cols.remove(at);
                cols.insert(to.min(cols.len()), c);
            }
            f.columns = cols;
        }
    });
    Ok(json!({"file": file.0, "column": key, "to": to}))
}

/// Resize a column (label `RESIZE_COLUMN`, merged while dragging).
pub fn view_resize_column(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::VIEW_RESIZE_COLUMN)?;
    let key = required(p, "column", ids::VIEW_RESIZE_COLUMN)?.to_string();
    let width = p.get("width").and_then(Value::as_f64).map(|w| w.clamp(16.0, 4000.0) as f32);
    let merge = format!("column-{}-{key}", file.0);
    s.edit_merged(labels::RESIZE_COLUMN, &merge, |d| {
        super::agent::person_acted(d);
        if let Some(f) = d.file_mut(file) {
            let mut cols = columns_of(f);
            match cols.iter_mut().find(|c| c.key == key) {
                Some(c) => c.width = width,
                None => cols.push(Column { key: key.clone(), visible: true, width }),
            }
            f.columns = cols;
        }
    });
    Ok(json!({"file": file.0, "column": key, "width": width}))
}

// ---------------------------------------------------------------------------------------------
// Bookmarks and highlights
// ---------------------------------------------------------------------------------------------

/// Bookmark the selected line (label `ADD_BOOKMARK`, by the person).
pub fn bookmark_here(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::BOOKMARK_HERE)?;
    let line = selected_line(s, file).ok_or_else(|| CommandError::failed(ids::BOOKMARK_HERE, "no line is selected"))?;
    if let Some(b) = s.doc().file(file).and_then(|f| f.bookmarks.iter().find(|b| b.line == line)) {
        return Ok(json!({"file": file.0, "bookmark": b.id, "line": line + 1, "name": b.name}));
    }
    let name = str_arg(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| format!("Line {}", line + 1), str::to_string);
    let id = edit(s, labels::ADD_BOOKMARK, |d| {
        let id = d.take_id();
        if let Some(f) = d.file_mut(file) {
            f.bookmarks.push(Bookmark { id, line, name: name.clone(), note: None, color: BookmarkColor::Amber, by: Actor::User });
            f.bookmarks.sort_by_key(|b| b.line);
        }
        id
    });
    Ok(json!({"file": file.0, "bookmark": id, "line": line + 1, "name": name}))
}

/// Remove a bookmark (label `REMOVE_BOOKMARK`).
pub fn bookmark_remove(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::BOOKMARK_REMOVE)?;
    let id = u64_arg(p, "id").ok_or_else(|| CommandError::bad_params(ids::BOOKMARK_REMOVE, "`id` is required"))?;
    if !s.doc().file(file).is_some_and(|f| f.bookmarks.iter().any(|b| b.id == id)) {
        return Err(CommandError::failed(ids::BOOKMARK_REMOVE, format!("no bookmark {id}")));
    }
    edit(s, labels::REMOVE_BOOKMARK, |d| {
        if let Some(f) = d.file_mut(file) {
            f.bookmarks.retain(|b| b.id != id);
        }
    });
    Ok(json!({"file": file.0, "id": id}))
}

/// Rename a bookmark (label `RENAME_BOOKMARK`).
pub fn bookmark_rename(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::BOOKMARK_RENAME)?;
    let id = u64_arg(p, "id").ok_or_else(|| CommandError::bad_params(ids::BOOKMARK_RENAME, "`id` is required"))?;
    let name = required(p, "name", ids::BOOKMARK_RENAME)?.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::bad_params(ids::BOOKMARK_RENAME, "the name is empty"));
    }
    if !s.doc().file(file).is_some_and(|f| f.bookmarks.iter().any(|b| b.id == id)) {
        return Err(CommandError::failed(ids::BOOKMARK_RENAME, format!("no bookmark {id}")));
    }
    edit(s, labels::RENAME_BOOKMARK, |d| {
        if let Some(b) = d.file_mut(file).and_then(|f| f.bookmarks.iter_mut().find(|b| b.id == id)) {
            b.name = name.clone();
        }
    });
    Ok(json!({"file": file.0, "id": id, "name": name}))
}

/// Remove a highlight (label `REMOVE_HIGHLIGHT`).
pub fn highlight_remove(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::HIGHLIGHT_REMOVE)?;
    let id = u64_arg(p, "id").ok_or_else(|| CommandError::bad_params(ids::HIGHLIGHT_REMOVE, "`id` is required"))?;
    if !s.doc().file(file).is_some_and(|f| f.highlights.iter().any(|h| h.id == id)) {
        return Err(CommandError::failed(ids::HIGHLIGHT_REMOVE, format!("no highlight {id}")));
    }
    edit(s, labels::REMOVE_HIGHLIGHT, |d| {
        if let Some(f) = d.file_mut(file) {
            f.highlights.retain(|h| h.id != id);
        }
    });
    Ok(json!({"file": file.0, "id": id}))
}

// ---------------------------------------------------------------------------------------------
// Detail pane
// ---------------------------------------------------------------------------------------------

/// The selected line's text (cut to `limits::MAX_LINE_BYTES`), its file and the format to read it
/// with.
fn selected_text(s: &S, id: &str) -> Result<(String, strata_core::FormatKind, Arc<LogFile>), CommandError> {
    let file = active_file(s).ok_or_else(|| CommandError::failed(id, "no log file is open"))?;
    let line = s.view::<u64>(view_keys::SELECTED_LINE).ok_or_else(|| CommandError::failed(id, "no line is selected"))?;
    let log = s.doc().live.get(file).ok_or_else(|| CommandError::failed(id, "the file is still opening"))?;
    let l = log.line(line, strata_core::limits::MAX_LINE_BYTES).ok_or_else(|| CommandError::failed(id, "that line is not indexed yet"))?;
    let kind = s.doc().file(file).and_then(|f| f.format.as_deref()).and_then(strata_core::FormatKind::from_id).unwrap_or_else(|| log.format());
    Ok((l.text().into_owned(), kind, log))
}

/// Copy the selected line.
pub fn detail_copy_line(s: &mut S, _p: &Value) -> R {
    let (text, ..) = selected_text(s, ids::DETAIL_COPY_LINE)?;
    req::push(s, Request::Copy { text: text.clone() });
    Ok(json!({"text": text}))
}

/// Copy the selected line as pretty JSON.
pub fn detail_copy_json(s: &mut S, _p: &Value) -> R {
    let (text, kind, log) = selected_text(s, ids::DETAIL_COPY_JSON)?;
    let pretty = crate::views::detail::as_json(&text, kind, &log).text;
    req::push(s, Request::Copy { text: pretty.clone() });
    Ok(json!({"text": pretty}))
}

/// Add a `field = value` filter chip (label `ADD_FILTER`).
pub fn detail_filter_by_value(s: &mut S, p: &Value) -> R {
    let file = file_arg(s, p, ids::DETAIL_FILTER_BY_VALUE)?;
    let field = required(p, "field", ids::DETAIL_FILTER_BY_VALUE)?.trim();
    let value = required(p, "value", ids::DETAIL_FILTER_BY_VALUE)?;
    let name_ok = field.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '@')
        && field.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'));
    if !name_ok {
        return Err(CommandError::bad_params(ids::DETAIL_FILTER_BY_VALUE, format!("`{field}` is not a field name")));
    }
    let text = format!("{field} = {}", quote(value));
    let chip = edit(s, labels::ADD_FILTER, |d| {
        let id = d.take_id();
        if let Some(f) = d.file_mut(file) {
            f.filters.push(FilterChip { id, text: text.clone(), enabled: true, by: Actor::User });
        }
        id
    });
    Ok(json!({"file": file.0, "id": chip, "filter": text}))
}

// ---------------------------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------------------------

/// Open the command palette (`Effect::OpenPalette`).
pub fn palette_open(s: &mut S, _p: &Value) -> R {
    s.request(Effect::OpenPalette);
    Ok(json!({}))
}

/// Open Settings at a section (`view_keys::SETTINGS`).
pub fn settings_open(s: &mut S, p: &Value) -> R {
    let section = str_arg(p, "section").unwrap_or("general").to_string();
    s.set_view(view_keys::SETTINGS, &section);
    Ok(json!({"section": section}))
}

/// Allow or forbid agent changes (`view_keys::AGENT_ALLOW_CHANGES`, saved in Settings).
pub fn agent_allow_changes(s: &mut S, p: &Value) -> R {
    let on = bool_arg(p, "on").ok_or_else(|| CommandError::bad_params(ids::AGENT_ALLOW_CHANGES, "`on` is required"))?;
    s.set_view(view_keys::AGENT_ALLOW_CHANGES, &on);
    Ok(json!({"allow_changes": on}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_only_when_needed() {
        assert_eq!(quote("payments"), "payments");
        assert_eq!(quote("r-403cca04"), "r-403cca04");
        assert_eq!(quote("connection refused"), "\"connection refused\"");
        assert_eq!(quote("and"), "\"and\"");
        assert_eq!(quote("-x"), "\"-x\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn saved_filters_join_the_chips_that_are_on() {
        let chip = |text: &str, enabled| FilterChip { id: 1, text: text.into(), enabled, by: Actor::User };
        let mut f = OpenFile::default();
        assert_eq!(chip_text_for_save(&f), "");
        f.filters = vec![chip("service = api", true)];
        assert_eq!(chip_text_for_save(&f), "service = api");
        f.filters.push(chip("status >= 500", true));
        f.filters.push(chip("x", false));
        assert_eq!(chip_text_for_save(&f), "(service = api) and (status >= 500)");
    }
}
