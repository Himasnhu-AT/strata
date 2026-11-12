//! The window: toolbar, sidebar, histogram strip, log table, detail pane, status bar, empty and
//! error states (docs/DESIGN.md). Owned by the `ui` stream: one file per part under `views/`.
//!
//! Views read the document and view state and dispatch commands; they never change the document
//! directly. Every control registers an automation id (`strata.<part>.<name>`). Each frame the
//! view first takes the requests commands left ([`req`]), then reconciles each file's jobs
//! ([`live`]), then draws. Nothing here asks for frames by itself: an idle window (even one
//! following a file that does not change) draws nothing.

pub mod detail;
pub mod fmt;
pub mod histogram;
pub mod live;
pub mod req;
pub mod settings;
pub mod sidebar;
pub mod states;
pub mod status;
pub mod table;
pub mod theme;
pub mod toolbar;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ashlar::fs::RecentFiles;
use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::dialog::{Dialog, DialogButton, DialogOutcome};
use strata_core::{FileId, FormatKind, LevelCounts, LogFile, Matcher};

use crate::commands::user::{self, active_file};
use crate::commands::{S, ids};
use crate::doc::{StrataDoc, view_keys};
use live::{FileRt, SearchSpec};
use req::{Request, Target, keys};

/// Settings key of "Allow the agent to change the view" (the agent stream's).
const ALLOW_SETTING: &str = crate::mcp::settings_keys::ALLOW_CHANGES;

/// What the view keeps between frames.
#[derive(Default)]
struct Window {
    installed: bool,
    files: HashMap<FileId, FileRt>,
    seen: HashSet<FileId>,
    table: table::TableState,
    detail: detail::DetailState,
    toolbar: toolbar::ToolbarState,
    sidebar: sidebar::SidebarUi,
    hist: histogram::HistState,
    goto: Option<String>,
    matcher: Option<(SearchSpec, Option<Matcher>)>,
    title: Option<(String, bool, u64)>,
    memory: Option<(std::time::Instant, u64)>,
}

/// Whether a real window (not the headless driver) is there to own a native dialog.
fn has_native_window(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().inner_rect.is_some())
}

/// The format to read `f` with: the person's choice, else the detected one.
fn kind_of(f: &crate::doc::OpenFile, log: &LogFile) -> FormatKind {
    f.format.as_deref().and_then(FormatKind::from_id).unwrap_or_else(|| log.format())
}

/// The view function given to `App::view`.
pub fn view() -> impl FnMut(&mut egui::Ui, &mut Cx<'_, StrataDoc>) {
    let mut w = Window::default();
    move |ui, cx| w.frame(ui, cx)
}

impl Window {
    fn frame(&mut self, ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
        if !self.installed {
            self.installed = true;
            theme::install(ui.ctx());
        }
        self.sync_settings(cx);
        self.requests(cx);
        self.drops(cx);
        self.track_files(cx);
        let active = active_file(cx.session());
        let hold = cx.view_or(keys::HOLD_JOBS, false);
        let search = cx.session().view_state().get(view_keys::SEARCH).cloned();
        let doc = cx.snapshot();
        for f in &doc.files {
            let kind = doc.live.get(f.id).map(|l| kind_of(f, &l));
            let spec = SearchSpec::from_view(search.as_ref(), f.id).filter(|_| Some(f.id) == active);
            let rt = self.files.entry(f.id).or_insert_with(|| FileRt::new(f.id));
            rt.reconcile(cx, f, kind, spec, hold);
        }
        self.escape(ui, cx, active);
        self.title(cx, active);
        // a job that ran inline may have opened a file: read the document again
        let doc = cx.snapshot();
        if doc.files.is_empty() {
            status::show(ui, cx, &status::StatusIn::default());
            egui::CentralPanel::default().frame(egui::Frame::new().fill(cx.tokens().surface.app)).show(ui, |ui| states::empty(ui, cx));
        } else {
            self.window(ui, cx, &doc, active);
        }
        self.dialogs(cx, &doc, active);
    }

    /// Settings ▸ AI into view state (once, `agent_panel::sync`); "Allow the agent to change the
    /// view" back into Settings when a command changes it.
    fn sync_settings(&mut self, cx: &mut Cx<'_, StrataDoc>) {
        crate::agent_panel::sync(cx);
        if let Some(on) = cx.view::<bool>(view_keys::AGENT_ALLOW_CHANGES)
            && cx.settings.get::<bool>(ALLOW_SETTING) != Some(on)
        {
            let _ = cx.settings.set(ALLOW_SETTING, &on);
        }
    }

    /// New files go to Recent; closed files stop their jobs.
    fn track_files(&mut self, cx: &mut Cx<'_, StrataDoc>) {
        let doc = cx.snapshot();
        let new: Vec<_> = doc.files.iter().filter(|f| !self.seen.contains(&f.id)).map(|f| (f.id, f.path.clone())).collect();
        if !new.is_empty() && cx.view::<String>(view_keys::SCENE).is_none() {
            let mut recent = RecentFiles::load(cx.settings, sidebar::RECENT, sidebar::RECENT_MAX);
            for (_, p) in &new {
                recent.push(p);
            }
            recent.store(cx.settings, sidebar::RECENT);
        }
        self.seen.extend(new.into_iter().map(|(id, _)| id));
        let open: HashSet<FileId> = doc.files.iter().map(|f| f.id).collect();
        let gone: Vec<FileId> = self.files.keys().filter(|id| !open.contains(id)).copied().collect();
        for id in gone {
            if let Some(mut rt) = self.files.remove(&id) {
                rt.cancel(cx.jobs, false);
            }
        }
    }

    /// Open `path` (a drop, the dialog); a failure becomes the "Unreadable" dialog.
    fn open(cx: &mut Cx<'_, StrataDoc>, path: &std::path::Path) {
        if let Err(e) = cx.execute(ids::LOG_OPEN_DIALOG, json!({"path": path.display().to_string()})) {
            cx.set_view(
                keys::OPEN_ERROR,
                &json!({"path": path.display().to_string(), "reason": states::reason(&e.to_string()), "detail": e.to_string()}),
            );
        }
    }

    fn drops(&mut self, cx: &mut Cx<'_, StrataDoc>) {
        let dropped: Vec<std::path::PathBuf> = cx.dropped_files().to_vec();
        for p in dropped {
            Self::open(cx, &p);
        }
    }

    fn requests(&mut self, cx: &mut Cx<'_, StrataDoc>) {
        for r in req::take(cx.session_mut()) {
            match r {
                Request::OpenDialog => {
                    if has_native_window(cx.ctx())
                        && let Ok(path) = ashlar::native::dialogs::FileDialog::new().title("Open Log").open_file()
                    {
                        Self::open(cx, &path);
                    }
                }
                Request::StopIndexing { file } => {
                    if let Some(rt) = self.files.get_mut(&FileId(file)) {
                        rt.cancel(cx.jobs, true);
                    }
                }
                Request::Copy { text } => cx.ctx().copy_text(text),
                Request::StepMatch { file, back } => self.step_match(cx, FileId(file), back),
                Request::Find { file, target, back, from, time } => Self::find(cx, FileId(file), target, back, from, time),
            }
        }
    }

    fn step_match(&mut self, cx: &mut Cx<'_, StrataDoc>, file: FileId, back: bool) {
        let Some(rt) = self.files.get(&file) else { return };
        let (matches, done) = {
            let d = rt.derived();
            (d.matches.clone(), d.search_done)
        };
        if matches.is_empty() {
            if done {
                cx.toasts.info("No matches");
            }
            return;
        }
        let sel = cx.view::<u64>(view_keys::SELECTED_LINE);
        let last = matches.len() - 1;
        let i = match (sel, back) {
            (None, false) => 0,
            (None, true) => last,
            (Some(l), false) => {
                let i = matches.lower_bound(l.saturating_add(1));
                if i > last { 0 } else { i }
            }
            (Some(l), true) => matches.lower_bound(l).checked_sub(1).unwrap_or(last),
        };
        if let Some(line) = matches.get(i) {
            user::select(cx.session_mut(), file, line);
        }
    }

    /// Next / previous error or warning, or the first line at a time: a job (inline for small
    /// files) that selects what it finds.
    fn find(cx: &mut Cx<'_, StrataDoc>, file: FileId, target: Target, back: bool, from: Option<u64>, time: Option<i64>) {
        let Some(log) = cx.doc().live.get(file) else { return };
        let filter = user::target_filter(target, time);
        let start = match (target, from) {
            (Target::Time, _) => 0,
            (_, Some(l)) => l,
            (_, None) if back => log.lines(),
            (_, None) => 0,
        };
        let what = match target {
            Target::Error => "error",
            Target::Warning => "warning",
            Target::Time => "line at that time",
        };
        let small = log.len() <= live::INLINE_BYTES;
        let job = cx.jobs.job("strata.find", format!("Finding the next {what}"));
        let job = if small { job.here() } else { job };
        job.spawn((log, filter), move |(log, filter), p| {
            let cancel = || p.is_cancelled();
            let found = strata_core::search::find_next(&log, &filter, start, back, &cancel).map_err(|e| e.to_string())?;
            Ok(apply(move |s: &mut S| {
                match found {
                    Some(line) => user::select(s, file, line),
                    None => {
                        let dir = if back { "before" } else { "after" };
                        let msg =
                            if target == Target::Time { "No line at or after that time".to_string() } else { format!("No {what} {dir} this line") };
                        s.toast(ToastLevel::Info, msg);
                    }
                }
                Ok(Value::Null)
            }))
        });
    }

    /// Esc stops indexing the active file (when no field has the keyboard).
    fn escape(&mut self, ui: &egui::Ui, cx: &mut Cx<'_, StrataDoc>, active: Option<FileId>) {
        let Some(id) = active else { return };
        let indexing = self.files.get(&id).is_some_and(|rt| rt.indexing(cx.jobs));
        if indexing && !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cx.dispatch(ids::LOG_STOP_INDEXING, json!({"file": id.0}));
        }
    }

    /// The window title: the file's name and folder (ashlar sets its own when the workspace
    /// changes, so it is sent again then).
    fn title(&mut self, cx: &mut Cx<'_, StrataDoc>, active: Option<FileId>) {
        let Some(f) = active.and_then(|id| cx.doc().file(id)) else { return };
        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let dir = f.path.parent().map(fmt::tilde).unwrap_or_default();
        let text = format!("{name} — {dir} — Strata");
        let key = (text.clone(), cx.session().is_dirty(), cx.session().file_generation());
        if self.title.as_ref() != Some(&key) {
            cx.ctx().send_viewport_cmd(egui::ViewportCommand::Title(text));
            self.title = Some(key);
        }
    }

    /// The process's memory, read at most once a second (and only on frames that run anyway).
    fn memory(&mut self) -> Option<u64> {
        let now = std::time::Instant::now();
        match self.memory {
            Some((at, m)) if now.duration_since(at).as_secs() < 1 => Some(m),
            _ => {
                let m = ashlar::native::memory::process_memory()?.footprint;
                self.memory = Some((now, m));
                Some(m)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn window(&mut self, ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, doc: &Arc<StrataDoc>, active: Option<FileId>) {
        let t = cx.tokens();
        let f = active.and_then(|id| doc.file(id));
        let log = active.and_then(|id| doc.live.get(id));
        let in_scene = cx.view::<String>(view_keys::SCENE).is_some();
        let mut levels = LevelCounts::default();
        let mut indexing = None;
        let mut rows_info = None;
        let mut counter = toolbar::Counter { current: None, total: 0, done: true, error: None };
        let mut hist = None;
        let mut load_error = None;
        let mut change = None;
        if let Some(rt) = active.and_then(|id| self.files.get_mut(&id)) {
            let running = rt.indexing(cx.jobs);
            let d = rt.derived();
            load_error.clone_from(&d.load_error);
            change = d.change;
            hist.clone_from(&d.hist);
            rows_info = Some((d.rows.clone(), d.rows_done, d.rows_error.clone()));
            let sel = cx.view::<u64>(view_keys::SELECTED_LINE);
            counter = toolbar::Counter {
                current: sel.and_then(|l| d.matches.position(l)).map(|i| i as u64 + 1),
                total: d.match_total.max(d.matches.len() as u64),
                done: d.search_done,
                error: d.search_error.clone(),
            };
            let progress = d.progress;
            drop(d);
            if let Some(log) = &log {
                levels = rt.levels(log);
                if let strata_core::IndexState::Running { done, total } = log.index_state() {
                    let frac = progress.unwrap_or(if total == 0 { 0.0 } else { done as f32 / total as f32 });
                    indexing = Some((frac, done, total));
                } else if running && log.lines() == 0 {
                    indexing = Some((0.0, 0, log.len()));
                }
            }
        }
        if let Some(f) = f
            && let Some(v) = cx.view::<Value>(keys::FILE_CHANGED)
            && v.get("file").and_then(Value::as_u64) == Some(f.id.0)
        {
            change = match v.get("change").and_then(Value::as_str) {
                Some("removed") => Some(strata_core::FileChange::Removed),
                _ => Some(strata_core::FileChange::Replaced),
            };
        }

        // status bar
        let filters = f.map_or(0, |f| {
            f.filters.iter().filter(|c| c.enabled).count()
                + usize::from(!f.hidden_levels.is_empty())
                + usize::from(f.time_from.is_some() || f.time_to.is_some())
        });
        let shown = rows_info.as_ref().and_then(|(rows, ..): &(Option<live::Lines>, bool, Option<String>)| rows.as_ref().map(|r| r.len() as u64));
        let s = status::StatusIn {
            lines: log.as_ref().map(|l| l.lines()),
            bytes: log.as_ref().map(|l| l.len()),
            format: log.as_ref().zip(f).map(|(l, f)| kind_of(f, l).label()),
            filters,
            shown,
            following: f.is_some_and(|f| f.follow),
            indexing,
            memory: if in_scene { None } else { self.memory() },
        };
        status::show(ui, cx, &s);

        // toolbar
        egui::Panel::top("strata.toolbar").exact_size(theme::TOOLBAR_HEIGHT).frame(egui::Frame::new().fill(t.surface.panel)).show(ui, |ui| {
            toolbar::show(ui, cx, &mut self.toolbar, f, &levels, &counter);
        });
        // sidebar
        egui::Panel::left("strata.sidebar.panel")
            .exact_size(theme::SIDEBAR_WIDTH)
            .resizable(false)
            .frame(egui::Frame::new().fill(t.surface.panel).inner_margin(egui::Margin::symmetric(8, 0)))
            .show(ui, |ui| sidebar::show(ui, cx, &mut self.sidebar, active));
        // the agent panel (the agent stream draws its inside)
        if cx.view_or(view_keys::AGENT_PANEL_OPEN, false) {
            egui::Panel::right("strata.agent.panel")
                .exact_size(crate::agent_panel::WIDTH)
                .resizable(false)
                .frame(egui::Frame::new().fill(t.surface.panel).inner_margin(egui::Margin::same(12)))
                .show(ui, |ui| crate::agent_panel::show(ui, cx));
        }
        let Some(f) = f else { return };
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.surface.app)).show(ui, |ui| {
            let Some(log) = log.clone() else {
                match &load_error {
                    Some(e) => states::load_error(ui, cx, f.id.0, &f.path, &states::reason(e)),
                    None => {
                        ui.add_space(ui.available_height() * 0.4);
                        ui.vertical_centered(|ui| {
                            let r = ui.label(egui::RichText::new("Opening…").color(t.text.secondary));
                            automation::register(ui, &r, "strata.table.opening", "Opening", Role::Status);
                        });
                    }
                }
                return;
            };
            let kind = kind_of(f, &log);
            // histogram strip
            egui::Panel::top("strata.histogram.panel").exact_size(theme::HISTOGRAM_HEIGHT).frame(egui::Frame::new()).show_separator_line(false).show(
                ui,
                |ui| {
                    histogram::show(ui, cx, &mut self.hist, f.id.0, hist.as_ref(), self.table.visible_times);
                },
            );
            if let Some(c) = change {
                states::changed_banner(ui, cx, f.id.0, c == strata_core::FileChange::Removed);
            }
            if let Some((frac, done, total)) = indexing {
                states::indexing_bar(ui, cx, f.id.0, frac, done, total, log.lines());
            }
            // detail pane
            let selected = cx.view::<u64>(view_keys::SELECTED_LINE).filter(|l| *l < log.lines());
            if cx.view_or(view_keys::DETAIL_OPEN, true) {
                egui::Panel::bottom("strata.detail.panel")
                    .default_size(theme::DETAIL_HEIGHT)
                    .size_range(120.0..=600.0)
                    .resizable(true)
                    .frame(egui::Frame::new().fill(t.surface.panel))
                    .show(ui, |ui| detail::show(ui, cx, &mut self.detail, &log, kind, selected));
            }
            // the table
            let (rows, rows_done, rows_error) = rows_info.clone().unwrap_or((None, true, None));
            let scroll_to = cx.view::<u64>(view_keys::SCROLL_TO);
            if scroll_to.is_some() {
                cx.session_mut().set_view_value(view_keys::SCROLL_TO, Value::Null);
            }
            if log.lines() == 0 {
                ui.add_space(ui.available_height() * 0.4);
                ui.vertical_centered(|ui| {
                    let text = if indexing.is_some() { "Reading the first lines…" } else { "This file is empty" };
                    let r = ui.label(egui::RichText::new(text).color(t.text.secondary));
                    automation::register(ui, &r, "strata.table.empty", text, Role::Status);
                });
                return;
            }
            let map = match rows {
                Some(r) => {
                    if r.is_empty() && (rows_done || rows_error.is_some()) {
                        table::no_matches(ui, cx, f, rows_error.as_deref());
                        return;
                    }
                    table::RowMap::Filtered(r)
                }
                None => {
                    let total = log.lines();
                    let start = self.table.window(f.id, total, scroll_to);
                    let len = usize::try_from(total.saturating_sub(start)).unwrap_or(usize::MAX).min(ashlar::ui::virtual_list::MAX_ROWS);
                    table::RowMap::Window { start, len, total }
                }
            };
            let spec = SearchSpec::from_view(cx.session().view_state().get(view_keys::SEARCH), f.id);
            if self.matcher.as_ref().map(|(s, _)| s) != spec.as_ref() {
                self.matcher = spec.map(|s| {
                    let m = s.query.compile().ok();
                    (s, m)
                });
            }
            let matcher = self.matcher.as_ref().and_then(|(_, m)| m.as_ref());
            let inp = table::TableIn { open: f, file: &log, kind, rows: map, selected, scroll_to, matcher, follow: f.follow };
            table::show(ui, cx, &mut self.table, inp);
        });
    }

    fn dialogs(&mut self, cx: &mut Cx<'_, StrataDoc>, doc: &Arc<StrataDoc>, active: Option<FileId>) {
        let ctx = cx.ctx().clone();
        // Unreadable
        if let Some(v) = cx.view::<Value>(keys::OPEN_ERROR) {
            let path = v.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or_default().to_string();
            let detail = v.get("detail").and_then(Value::as_str).unwrap_or_default().to_string();
            if states::unreadable_dialog(&ctx, &path, &reason, &detail) {
                cx.set_view(keys::OPEN_ERROR, &Value::Null);
            }
        }
        // Settings
        let f = active.and_then(|id| doc.file(id));
        let detected = active.and_then(|id| doc.live.get(id)).map(|l| l.format());
        settings::show(cx, f, detected);
        // Go to line or time
        if cx.view::<String>(view_keys::FOCUS).as_deref() == Some("goto") {
            cx.set_view(view_keys::FOCUS, &Value::Null);
            if active.is_some() {
                self.goto = Some(String::new());
            }
        }
        if let (Some(text), Some(file)) = (self.goto.as_mut(), active) {
            let buttons = [DialogButton::cancel("Cancel"), DialogButton::new("Go").default()];
            let out = Dialog::new("strata.goto", "Go to line or time", &buttons).width(360.0).show(&ctx, |ui| {
                let r = ui.add(egui::TextEdit::singleline(text).hint_text("841203 or 2026-11-12T14:02:00Z").desired_width(f32::INFINITY));
                automation::register(ui, &r, "strata.goto.field", "Line number or time", Role::Field);
            });
            match out {
                Some(DialogOutcome::Button(1)) => {
                    let text = text.clone();
                    self.goto = None;
                    if let Err(e) = user::goto_text(cx.session_mut(), file, &text) {
                        cx.toasts.error(e.to_string());
                    }
                }
                Some(_) => self.goto = None,
                None => {}
            }
        }
    }
}
