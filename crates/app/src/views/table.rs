//! The log table: line number, time, level, service (or the first parsed field), message, plus any
//! field the person shows. Virtual (`VirtualTable`): a frame lays out only the visible rows, which
//! are read from the map and parsed on demand, with a small cache.
//!
//! **More than 10 M rows.** ashlar 0.2's `VirtualTable` holds at most
//! [`MAX_ROWS`](ashlar::ui::virtual_list::MAX_ROWS) (10 M) rows. A bigger file is shown as
//! *windows* of 10 M lines: a bar above the table says "Lines 10,000,001–20,000,000 of
//! 52,113,004" with Previous and Next, and going to a line (Cmd+G, a bookmark, next error, the
//! agent's `view.goto`) moves the window so that line is in it. A filter keeps the first 10 M
//! matching lines and says so.
//!
//! **Columns.** `VirtualTable` has no column reorder or hide in 0.2: the table is rebuilt from the
//! document's columns (`view.toggle_column`, `view.move_column`), offered by a right click on the
//! header. Widths dragged in the header are saved with `view.resize_column` when the drag ends.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Details, Role};
use ashlar::ui::overlay::Popover;
use ashlar::ui::tokens::Tokens;
use ashlar::ui::virtual_list::{Column as TableColumn, MAX_ROWS, RowHeights, VirtualTable};
use strata_core::{FileId, FormatKind, Level, LogFile, Matcher, Timestamp};

use super::live::Lines;
use super::theme::{self, ROW_HEIGHT};
use super::{detail, fmt};
use crate::commands::ids;
use crate::commands::user::columns_of;
use crate::doc::{Actor, Column, OpenFile, StrataDoc, view_keys};

/// Bytes of a line read for a row (the rest is cut; the detail pane reads up to 64 KB).
const ROW_BYTES: usize = 4096;
/// Rows parsed and kept.
const CACHE_ROWS: usize = 2048;

/// Field names shown as the "service" column when present.
pub fn is_service(name: &str) -> bool {
    matches!(name, "service" | "svc" | "app" | "application" | "component" | "logger" | "service.name" | "source" | "host")
}

/// Field names that are already the time, level or message column.
fn is_builtin(name: &str) -> bool {
    matches!(name, "ts" | "time" | "timestamp" | "@timestamp" | "level" | "lvl" | "severity" | "msg" | "message" | "@message")
}

/// One parsed row.
#[derive(Debug, Default)]
pub struct RowData {
    pub text: String,
    pub time: Option<Timestamp>,
    pub level: Option<Level>,
    pub message: String,
    pub fields: Vec<(String, String)>,
    /// What a screen reader says for the row.
    pub label: String,
}

impl RowData {
    fn read(file: &LogFile, kind: FormatKind, line: u64) -> RowData {
        let Some(l) = file.line(line, ROW_BYTES) else {
            return RowData { label: format!("Line {}", line + 1), ..RowData::default() };
        };
        let text = l.text().into_owned();
        let parsed = strata_core::formats::parser(kind).parse(&l.bytes, file.names());
        let fields = detail::fields(&text, &parsed, file);
        let message = text.get(parsed.message.start as usize..parsed.message.end as usize).filter(|m| !m.trim().is_empty()).unwrap_or(&text);
        let mut message = message.replace(['\t', '\r'], " ");
        if l.cut {
            message.push('…');
        }
        let mut label = format!("Line {}", line + 1);
        if let Some(lv) = parsed.level {
            label.push_str(", ");
            label.push_str(lv.as_str());
        }
        if let Some(t) = parsed.time {
            label.push_str(", ");
            label.push_str(&fmt::time_of_day(t));
        }
        label.push_str(", ");
        label.push_str(message.get(..message.floor_char_boundary(200)).unwrap_or_default());
        RowData { time: parsed.time, level: parsed.level, message, fields, label, text }
    }

    fn field(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Which line each table row shows.
#[derive(Clone, Debug)]
pub enum RowMap {
    /// Lines `start..start + len` (no filter).
    Window { start: u64, len: usize, total: u64 },
    /// The lines passing the filters.
    Filtered(Lines),
}

impl RowMap {
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        match self {
            RowMap::Window { len, .. } => *len,
            RowMap::Filtered(l) => l.len().min(MAX_ROWS),
        }
    }

    pub fn line(&self, row: usize) -> Option<u64> {
        match self {
            RowMap::Window { start, len, .. } => (row < *len).then(|| start.saturating_add(row as u64)),
            RowMap::Filtered(l) => l.get(row),
        }
    }

    pub fn row_of(&self, line: u64) -> Option<usize> {
        match self {
            RowMap::Window { start, len, .. } => line.checked_sub(*start).and_then(|d| usize::try_from(d).ok()).filter(|d| d < len),
            RowMap::Filtered(l) => l.position(line).filter(|r| *r < MAX_ROWS),
        }
    }
}

/// The window start (0-based line) that shows `line`, for a file of `total` lines.
pub fn window_for(line: u64, total: u64) -> u64 {
    let cap = MAX_ROWS as u64;
    if total <= cap {
        return 0;
    }
    let start = (line / cap) * cap;
    start.min(total.saturating_sub(cap))
}

/// One shown column.
#[derive(Clone, Debug, PartialEq)]
pub struct Col {
    pub key: String,
    pub title: String,
    pub width: f32,
    /// The width saved in the document (`None`: automatic).
    pub saved: Option<f32>,
}

fn title_of(key: &str) -> String {
    match key {
        "line" => "Line".into(),
        "time" => "Time".into(),
        "level" => "Level".into(),
        "message" => "Message".into(),
        other => other.to_string(),
    }
}

/// The columns to show: the document's, plus `extra` (the service field) before the message when
/// the document doesn't mention it.
pub fn shown_columns(f: &OpenFile, extra: Option<&str>, lines: u64, avail: f32) -> Vec<Col> {
    let mut cols: Vec<Column> = columns_of(f);
    if let Some(extra) = extra
        && !cols.iter().any(|c| c.key == extra)
    {
        let at = cols.iter().position(|c| c.key == "message").unwrap_or(cols.len());
        cols.insert(at, Column { key: extra.to_string(), visible: true, width: None });
    }
    let digits = fmt::thousands(lines.max(1000)).len() as f32;
    let auto = |key: &str| match key {
        "line" => (digits * 7.5 + 22.0).max(64.0),
        "time" => 104.0,
        "level" => 64.0,
        "message" => 0.0,
        _ => 112.0,
    };
    let mut out: Vec<Col> = cols
        .iter()
        .filter(|c| c.visible)
        .map(|c| Col { key: c.key.clone(), title: title_of(&c.key), width: c.width.unwrap_or_else(|| auto(&c.key)), saved: c.width })
        .collect();
    let used: f32 = out.iter().filter(|c| !(c.key == "message" && c.saved.is_none())).map(|c| c.width).sum();
    for c in out.iter_mut().filter(|c| c.key == "message" && c.saved.is_none()) {
        c.width = (avail - used - 4.0).max(320.0);
    }
    out
}

/// What the table keeps between frames.
#[derive(Default)]
pub struct TableState {
    cache: HashMap<u64, Arc<RowData>>,
    cache_key: Option<(u64, usize, FormatKind)>,
    /// Window start per file (files over 10 M lines).
    windows: HashMap<FileId, u64>,
    /// The last row was visible: following keeps the newest line in view.
    at_bottom: bool,
    rows_before: usize,
    /// Columns as last given to the table (widths reset when they change).
    cols_key: Option<String>,
    passed: Vec<f32>,
    columns_menu: bool,
    columns_anchor: Option<egui::Rect>,
    /// Field names of the file (for the columns menu), by file.
    field_names: Vec<String>,
    /// Rows laid out last frame, and the times of the first and last (the histogram bracket).
    pub visible: Range<usize>,
    pub visible_times: Option<(Timestamp, Timestamp)>,
}

impl TableState {
    fn row(&mut self, file: &LogFile, kind: FormatKind, line: u64) -> Arc<RowData> {
        let key = (file.id().0, file as *const LogFile as usize, kind);
        if self.cache_key != Some(key) || self.cache.len() > CACHE_ROWS {
            self.cache.clear();
            self.cache_key = Some(key);
        }
        Arc::clone(self.cache.entry(line).or_insert_with(|| Arc::new(RowData::read(file, kind, line))))
    }

    /// Forget parsed rows (the file was truncated or reloaded).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// The service-like field of the file, read from its first lines.
    pub fn service_field(&mut self, file: &LogFile, kind: FormatKind) -> Option<String> {
        let n = file.lines().min(8);
        let mut first_other = None;
        for line in 0..n {
            let row = self.row(file, kind, line);
            if let Some((name, _)) = row.fields.iter().find(|(n, _)| is_service(n)) {
                return Some(name.clone());
            }
            if first_other.is_none() {
                first_other = row.fields.iter().find(|(n, _)| !is_builtin(n)).map(|(n, _)| n.clone());
            }
        }
        first_other
    }

    /// The window start for `file` (files over 10 M lines), moved to show `line` when given.
    pub fn window(&mut self, file: FileId, total: u64, show: Option<u64>) -> u64 {
        let w = self.windows.entry(file).or_insert(0);
        if let Some(line) = show {
            let cap = MAX_ROWS as u64;
            if line < *w || line >= w.saturating_add(cap) {
                *w = window_for(line, total);
            }
        }
        *w = (*w).min(total.saturating_sub(MAX_ROWS as u64));
        *w
    }

    fn move_window(&mut self, file: FileId, total: u64, forward: bool) {
        let w = self.windows.entry(file).or_insert(0);
        let cap = MAX_ROWS as u64;
        *w = if forward { w.saturating_add(cap).min(total.saturating_sub(cap)) } else { w.saturating_sub(cap) };
    }
}

/// Everything the table needs this frame.
pub struct TableIn<'a> {
    pub open: &'a OpenFile,
    pub file: &'a Arc<LogFile>,
    pub kind: FormatKind,
    pub rows: RowMap,
    pub selected: Option<u64>,
    pub scroll_to: Option<u64>,
    pub matcher: Option<&'a Matcher>,
    pub follow: bool,
}

fn mono(t: &Tokens) -> egui::FontId {
    egui::FontId::monospace(t.type_scale.mono)
}

fn cell_text(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false).truncate());
}

/// A message with its search matches underlined in amber.
fn message_job(t: &Tokens, text: &str, matcher: Option<&Matcher>, color: egui::Color32) -> egui::text::LayoutJob {
    let font = mono(t);
    let plain = egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    let mut spans = Vec::new();
    if let Some(m) = matcher {
        m.find_all(text.as_bytes(), &mut spans);
    }
    let mut at = 0usize;
    for s in spans {
        let (a, b) = (s.start as usize, s.end as usize);
        if a < at || b <= a || !text.is_char_boundary(a) || !text.is_char_boundary(b) || b > text.len() {
            continue;
        }
        job.append(text.get(at..a).unwrap_or_default(), 0.0, plain.clone());
        let hit = egui::TextFormat { underline: egui::Stroke::new(1.5, t.accent), ..plain.clone() };
        job.append(text.get(a..b).unwrap_or_default(), 0.0, hit);
        at = b;
    }
    job.append(text.get(at..).unwrap_or_default(), 0.0, plain);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

/// Paint `color` behind a whole cell (the cell's ui is clipped to its padded inside).
fn cell_background(ui: &egui::Ui, t: &Tokens, color: egui::Color32) {
    let cell = ui.max_rect().expand2(egui::vec2(t.space.sm, 0.0));
    let clip = egui::Rect::from_x_y_ranges(cell.x_range(), ui.clip_rect().y_range());
    let row = egui::Rect::from_x_y_ranges(cell.x_range(), ui.max_rect().expand2(egui::vec2(0.0, ROW_HEIGHT)).y_range()).intersect(clip);
    let r = egui::Rect::from_x_y_ranges(cell.x_range(), row.y_range());
    ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(clip).rect_filled(r, 0.0, color);
}

/// Draw the table and the bar above it (files over 10 M lines). Returns the rows shown.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, st: &mut TableState, inp: TableIn<'_>) {
    let t = cx.tokens();
    let file_id = inp.open.id;
    let total = inp.file.lines();
    // the window bar
    if let RowMap::Window { start, len, total } = inp.rows
        && total > MAX_ROWS as u64
    {
        window_bar(ui, cx, st, file_id, start, len, total);
    }
    if total == 0 {
        return;
    }
    let extra = st.service_field(inp.file, inp.kind);
    let avail = ui.available_width();
    let cols = shown_columns(inp.open, extra.as_deref(), total, avail);
    let key: String = cols.iter().map(|c| format!("{}:{:?};", c.key, c.saved)).collect();
    let table_id = "strata.table";
    if st.cols_key.as_deref() != Some(&key) {
        let widths_id = ui.make_persistent_id(table_id).with("widths");
        ui.data_mut(|d| d.remove_temp::<Vec<f32>>(widths_id));
        st.cols_key = Some(key);
        st.passed = cols.iter().map(|c| c.width).collect();
    }
    let titles: Vec<String> = cols.iter().map(|c| c.title.clone()).collect();
    let columns: Vec<TableColumn<'_>> = cols.iter().zip(&titles).map(|(c, title)| TableColumn::new(title, c.width)).collect();

    let rows = inp.rows.len();
    let selected_row = inp.selected.and_then(|l| inp.rows.row_of(l));
    let mut scroll = inp.scroll_to.and_then(|l| inp.rows.row_of(l));
    if inp.follow && st.at_bottom && rows > st.rows_before && rows > 0 {
        scroll = Some(rows - 1);
    }
    st.rows_before = rows;
    let highlights: Vec<(&[u64], Actor)> = inp.open.highlights.iter().map(|h| (h.lines.as_slice(), h.by)).collect();
    let highlighted = |line: u64| highlights.iter().find(|(l, _)| l.binary_search(&line).is_ok()).map(|(_, by)| *by);
    let bookmark = |line: u64| inp.open.bookmarks.iter().find(|b| b.line == line);

    let header_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), t.control_height));
    // what a screen reader says for each row near last frame's visible ones
    let around = st.visible.start.saturating_sub(8)..st.visible.end.saturating_add(8).min(rows);
    let labels: HashMap<usize, String> = around.filter_map(|r| Some((r, st.row(inp.file, inp.kind, inp.rows.line(r)?).label.clone()))).collect();
    let row_label = |r: usize| -> &str { labels.get(&r).map_or("", String::as_str) };
    let file = Arc::clone(inp.file);
    let kind = inp.kind;
    let map = inp.rows.clone();
    let out = VirtualTable::new(table_id, &columns, rows, RowHeights::Uniform(ROW_HEIGHT))
        .label("Log lines")
        .selected(selected_row)
        .scroll_to(scroll)
        .row_label(&row_label)
        .show(ui, |ui, row, c| {
            let Some(line) = map.line(row) else { return };
            let data = st.row(&file, kind, line);
            let Some(col) = cols.get(c) else { return };
            let is_selected = Some(row) == selected_row;
            let hl = highlighted(line);
            if !is_selected {
                if hl.is_some() {
                    cell_background(ui, &t, theme::tint(t.accent, 26));
                } else if matches!(data.level, Some(Level::Error | Level::Fatal)) {
                    cell_background(ui, &t, theme::tint(t.status.danger, 20));
                }
            }
            match col.key.as_str() {
                "line" => {
                    if hl.is_some() && c == 0 {
                        let r = ui.max_rect().expand2(egui::vec2(t.space.sm, 0.0));
                        let bar = egui::Rect::from_min_size(r.left_top(), egui::vec2(2.0, ROW_HEIGHT.min(r.height().max(ROW_HEIGHT))));
                        ui.ctx()
                            .layer_painter(ui.layer_id())
                            .with_clip_rect(egui::Rect::from_x_y_ranges(r.x_range(), ui.clip_rect().y_range()))
                            .rect_filled(bar, 0.0, t.accent);
                    }
                    if let Some(b) = bookmark(line) {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, theme::bookmark_color(&t, b.color));
                    }
                    cell_text(ui, egui::RichText::new(fmt::thousands(line + 1)).font(mono(&t)).color(t.text.disabled));
                }
                "time" => {
                    let s = data.time.map(fmt::time_of_day).unwrap_or_default();
                    cell_text(ui, egui::RichText::new(s).font(mono(&t)).color(t.text.secondary));
                }
                "level" => {
                    let word = data.level.map(Level::as_str).unwrap_or_default();
                    cell_text(ui, egui::RichText::new(word).font(mono(&t)).color(theme::level_color(&t, data.level)));
                }
                "message" => {
                    if hl == Some(Actor::Agent) {
                        let tag = egui::RichText::new("AI").size(10.0).color(t.text.on_accent).background_color(t.accent);
                        ui.add(egui::Label::new(tag).selectable(false));
                    }
                    let job = message_job(&t, &data.message, inp.matcher, t.text.primary);
                    ui.add(egui::Label::new(job).selectable(false).truncate());
                }
                field => {
                    let v = data.field(field).unwrap_or_default();
                    cell_text(ui, egui::RichText::new(v).font(mono(&t)).color(t.text.primary));
                }
            }
        });
    st.at_bottom = out.list.visible.end >= rows;
    st.visible = out.list.visible.clone();
    st.visible_times = visible_lines(&inp.rows, &st.visible).and_then(|(a, b)| {
        let ta = st.row(inp.file, inp.kind, a).time?;
        let tb = st.row(inp.file, inp.kind, b).time?;
        Some((ta.min(tb), ta.max(tb)))
    });
    if let Some(row) = out.list.select
        && let Some(line) = inp.rows.line(row)
    {
        cx.dispatch(ids::VIEW_SELECT, json!({"file": file_id.0, "line": line + 1}));
    }
    if out.list.activated.is_some() && !cx.view_or(view_keys::DETAIL_OPEN, true) {
        cx.dispatch(ids::VIEW_TOGGLE_DETAIL, json!({}));
    }
    // widths dragged in the header: saved when the drag ends
    let dragging = ui.input(|i| i.pointer.any_down());
    if !dragging && out.widths.len() == st.passed.len() {
        for ((c, w), was) in cols.iter().zip(&out.widths).zip(&st.passed) {
            if (w - was).abs() > 0.5 {
                cx.dispatch(ids::VIEW_RESIZE_COLUMN, json!({"file": file_id.0, "column": c.key, "width": w}));
            }
        }
        st.passed.clone_from(&out.widths);
    }
    // right click on the header: the columns menu
    let header = ui.interact(header_rect, ui.make_persistent_id("strata.table.header_menu"), egui::Sense::hover());
    if ui.input(|i| i.pointer.secondary_clicked()) && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| header_rect.contains(p)) {
        st.columns_menu = true;
        st.columns_anchor = Some(header_rect);
        st.field_names = inp.file.names().all();
    }
    columns_menu(cx, st, &header, inp.open, extra.as_deref());
}

fn window_bar(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, st: &mut TableState, file: FileId, start: u64, len: usize, total: u64) {
    let t = cx.tokens();
    egui::Frame::new().fill(theme::tint(t.accent, 18)).inner_margin(egui::Margin::symmetric(t.space.xl as i8, t.space.sm as i8)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let text = format!(
                "Lines {}–{} of {} · the table holds {} rows at a time; Go to line (⌘G) moves it",
                fmt::thousands(start + 1),
                fmt::thousands(start + len as u64),
                fmt::thousands(total),
                fmt::thousands(MAX_ROWS as u64)
            );
            let r = ui.label(egui::RichText::new(&text).color(t.text.primary));
            automation::register(ui, &r, "strata.table.window", &text, Role::Label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let more = start.saturating_add(len as u64) < total;
                if ashlar::ui::controls::Button::new("strata.table.window.next", "Next ▸")
                    .size(ashlar::ui::controls::Size::Sm)
                    .enabled(more)
                    .show(ui)
                    .clicked()
                {
                    st.move_window(file, total, true);
                }
                if ashlar::ui::controls::Button::new("strata.table.window.prev", "◂ Previous")
                    .size(ashlar::ui::controls::Size::Sm)
                    .enabled(start > 0)
                    .show(ui)
                    .clicked()
                {
                    st.move_window(file, total, false);
                }
            });
        });
    });
}

fn columns_menu(cx: &mut Cx<'_, StrataDoc>, st: &mut TableState, anchor: &egui::Response, f: &OpenFile, extra: Option<&str>) {
    if !st.columns_menu {
        return;
    }
    let mut cols = columns_of(f);
    if let Some(extra) = extra
        && !cols.iter().any(|c| c.key == extra)
    {
        let at = cols.iter().position(|c| c.key == "message").unwrap_or(cols.len());
        cols.insert(at, Column { key: extra.to_string(), visible: true, width: None });
    }
    for name in &st.field_names {
        if !cols.iter().any(|c| &c.key == name) && !is_builtin(name) {
            cols.push(Column { key: name.clone(), visible: false, width: None });
        }
    }
    let file = f.id.0;
    let mut open = st.columns_menu;
    Popover::new("strata.table.columns").width(240.0).show(anchor, &mut open, |ui| {
        let t = ashlar::ui::tokens::current(ui.ctx());
        ui.label(egui::RichText::new("Columns").strong());
        let n = cols.len();
        for (i, c) in cols.iter().enumerate().take(64) {
            ui.horizontal(|ui| {
                let mut on = c.visible;
                let id = format!("strata.table.columns.{}", c.key);
                let r = ui.checkbox(&mut on, title_of(&c.key));
                automation::register_with(ui, &r, &id, &title_of(&c.key), Role::Toggle, Details { value: None, selected: Some(c.visible) });
                if r.changed() {
                    cx.dispatch(ids::VIEW_TOGGLE_COLUMN, json!({"file": file, "column": c.key, "on": on}));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let down = ui.add_enabled(i + 1 < n, egui::Button::new(egui::RichText::new("↓").color(t.text.secondary)).frame(false));
                    if down.on_hover_text("Move right").clicked() {
                        cx.dispatch(ids::VIEW_MOVE_COLUMN, json!({"file": file, "column": c.key, "to": i + 1}));
                    }
                    let up = ui.add_enabled(i > 0, egui::Button::new(egui::RichText::new("↑").color(t.text.secondary)).frame(false));
                    if up.on_hover_text("Move left").clicked() {
                        cx.dispatch(ids::VIEW_MOVE_COLUMN, json!({"file": file, "column": c.key, "to": i - 1}));
                    }
                });
            });
        }
    });
    st.columns_menu = open;
}

/// The empty-table states: nothing matches the filters, the filter could not be read.
pub fn no_matches(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, f: &OpenFile, error: Option<&str>) {
    let t = cx.tokens();
    ui.add_space((ui.available_height() * 0.25).max(t.space.xl));
    ui.vertical_centered(|ui| {
        let title = "No lines match these filters";
        let r = ui.label(egui::RichText::new(title).size(t.type_scale.heading).color(t.text.primary));
        automation::register(ui, &r, "strata.table.no_matches", title, Role::Label);
        ui.add_space(t.space.sm);
        if let Some(e) = error {
            let text = format!("Filter not understood: {e}");
            let r = ui.label(egui::RichText::new(&text).color(t.status.danger));
            automation::register(ui, &r, "strata.table.filter_error", &text, Role::Alert);
            ui.add_space(t.space.sm);
        }
        let mut parts: Vec<String> = f.filters.iter().filter(|c| c.enabled).map(|c| c.text.clone()).collect();
        if !f.hidden_levels.is_empty() {
            parts.push(format!("hiding {}", f.hidden_levels.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if f.time_from.is_some() || f.time_to.is_some() {
            parts.push("a time range".into());
        }
        if !parts.is_empty() {
            let r = ui.label(egui::RichText::new(parts.join(" · ")).font(mono(&t)).color(t.text.secondary));
            automation::register(ui, &r, "strata.table.active_filters", &parts.join(" · "), Role::Label);
        }
        ui.add_space(t.space.lg);
        if ashlar::ui::controls::Button::new("strata.table.clear_filters", "Clear filters")
            .variant(ashlar::ui::controls::Variant::Primary)
            .show(ui)
            .clicked()
        {
            clear_filters(cx, f);
        }
    });
}

/// Clear every filter of `f`: `view.clear_filters` (one undo step), or one command per filter if
/// that is not available.
pub fn clear_filters(cx: &mut Cx<'_, StrataDoc>, f: &OpenFile) {
    let file = f.id.0;
    if cx.execute(ids::VIEW_CLEAR_FILTERS, json!({"file": file})).is_ok() {
        if !f.hidden_levels.is_empty() {
            for l in &f.hidden_levels {
                cx.dispatch(ids::FILTER_LEVEL, json!({"file": file, "level": l.as_str(), "on": true}));
            }
        }
        return;
    }
    for c in &f.filters {
        cx.dispatch(ids::FILTER_REMOVE, json!({"file": file, "id": c.id}));
    }
    for l in &f.hidden_levels {
        cx.dispatch(ids::FILTER_LEVEL, json!({"file": file, "level": l.as_str(), "on": true}));
    }
    if f.time_from.is_some() || f.time_to.is_some() {
        cx.dispatch(ids::FILTER_TIME_RANGE, json!({"file": file}));
    }
}

/// The range of rows `visible` as lines, for the histogram bracket.
pub fn visible_lines(map: &RowMap, visible: &Range<usize>) -> Option<(u64, u64)> {
    let first = map.line(visible.start)?;
    let last = map.line(visible.end.saturating_sub(1).max(visible.start))?;
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hold_ten_million_rows() {
        let cap = MAX_ROWS as u64;
        assert_eq!(window_for(5, 100), 0);
        assert_eq!(window_for(cap + 5, cap * 5), cap);
        assert_eq!(window_for(cap * 5 - 1, cap * 5 + 7), cap * 4);
        assert_eq!(window_for(cap * 5 + 3, cap * 5 + 7), cap * 4 + 7, "the last window ends at the last line");
        let mut st = TableState::default();
        let f = FileId(1);
        assert_eq!(st.window(f, cap * 3, None), 0);
        assert_eq!(st.window(f, cap * 3, Some(cap + 1)), cap);
        assert_eq!(st.window(f, cap * 3, Some(cap + 2)), cap, "a line in the window keeps it");
        st.move_window(f, cap * 3, true);
        assert_eq!(st.window(f, cap * 3, None), cap * 2);
        st.move_window(f, cap * 3, true);
        assert_eq!(st.window(f, cap * 3, None), cap * 2);
        let map = RowMap::Window { start: cap, len: MAX_ROWS, total: cap * 3 };
        assert_eq!(map.line(0), Some(cap));
        assert_eq!(map.row_of(cap + 3), Some(3));
        assert_eq!(map.row_of(3), None);
        assert_eq!(map.line(MAX_ROWS), None);
    }

    #[test]
    fn the_service_column_goes_before_the_message() {
        let f = OpenFile { columns: crate::doc::default_columns(), ..OpenFile::default() };
        let cols = shown_columns(&f, Some("service"), 360, 1000.0);
        let keys: Vec<&str> = cols.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["line", "time", "level", "service", "message"]);
        assert!(cols.iter().all(|c| c.width > 0.0));
        let mut hidden = f.clone();
        hidden.columns.insert(3, Column { key: "service".into(), visible: false, width: None });
        let keys: Vec<String> = shown_columns(&hidden, Some("service"), 360, 1000.0).into_iter().map(|c| c.key).collect();
        assert_eq!(keys, ["line", "time", "level", "message"]);
    }
}
