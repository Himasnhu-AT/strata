//! The detail pane. A JSON line is pretty-printed as written, with Copy line, Copy JSON and
//! Filter by this value. Any other line (plain text, logfmt, syslog …) shows the raw line as
//! written, then its parsed parts as a small table (time, level, service, message, `key=value`
//! fields), with Copy line and Filter by; there is no JSON block for it.
//!
//! **Multi-line entries.** The core indexes physical lines, so a stack trace's lines are rows of
//! their own. In timestamped text a line with no time and no level is a *continuation* of the
//! entry above it ([`is_continuation`]): the table marks it (a guide in the time column, dimmed
//! text, the entry's error tint), the pane of an entry shows its continuation lines under the
//! raw line, and the pane of a continuation line names the entry it belongs to.
//!
//! A click on a line of the JSON (or a row of the parts table) picks that field for "Filter
//! by"; the default is the service (or the first field).

use std::ops::Range;
use std::sync::Arc;

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::code_view::CodeView;
use ashlar::ui::controls::{Button, Size, Variant};
use strata_core::{FormatKind, LogFile, Parsed};

use super::fmt;
use crate::commands::ids;
use crate::doc::StrataDoc;

/// Pretty JSON and, per output line, the field it shows and its value (for "Filter by").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonView {
    pub text: String,
    pub fields: Vec<Option<(String, String)>>,
}

impl JsonView {
    fn push(&mut self, line: String, field: Option<(String, String)>) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(&line);
        self.fields.push(field);
    }

    /// The byte range of output line `i`.
    pub fn line_range(&self, i: usize) -> Option<Range<usize>> {
        let mut start = 0;
        for (n, l) in self.text.split('\n').enumerate() {
            if n == i {
                return Some(start..start + l.len());
            }
            start += l.len() + 1;
        }
        None
    }
}

const MAX_DEPTH: usize = 32;

/// A pretty-printer over JSON text that keeps the key order and number spelling of the source.
struct Pretty<'a> {
    s: &'a [u8],
    i: usize,
    out: JsonView,
}

impl Pretty<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    /// The raw text of the string starting at `self.i` (with its quotes).
    fn string(&mut self) -> &str {
        let start = self.i;
        self.i += 1;
        while let Some(&b) = self.s.get(self.i) {
            self.i += 1;
            match b {
                b'\\' => self.i += 1,
                b'"' => break,
                _ => {}
            }
        }
        std::str::from_utf8(self.s.get(start..self.i.min(self.s.len())).unwrap_or_default()).unwrap_or_default()
    }

    fn scalar(&mut self) -> &str {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace()) {
            self.i += 1;
        }
        std::str::from_utf8(self.s.get(start..self.i).unwrap_or_default()).unwrap_or_default()
    }

    fn comma(&mut self) {
        if self.peek() == Some(b',') {
            self.i += 1;
            // the comma ends the line just written
            self.out.text.push(',');
        }
    }

    fn value(&mut self, indent: usize, prefix: String, path: Option<String>, depth: usize) {
        let pad = "  ".repeat(indent);
        match self.peek() {
            Some(open @ (b'{' | b'[')) if depth < MAX_DEPTH => {
                let close = if open == b'{' { b'}' } else { b']' };
                self.i += 1;
                if self.peek() == Some(close) {
                    self.i += 1;
                    self.out.push(format!("{pad}{prefix}{}{}", open as char, close as char), None);
                    return;
                }
                self.out.push(format!("{pad}{prefix}{}", open as char), None);
                let mut n = 0usize;
                while let Some(b) = self.peek() {
                    if b == close {
                        self.i += 1;
                        break;
                    }
                    if open == b'{' {
                        if b != b'"' {
                            return;
                        }
                        let key = self.string().to_string();
                        if self.peek() == Some(b':') {
                            self.i += 1;
                        }
                        let name = serde_json::from_str::<String>(&key).unwrap_or_default();
                        let sub = match &path {
                            Some(p) => format!("{p}.{name}"),
                            None => name,
                        };
                        self.value(indent + 1, format!("{key}: "), Some(sub), depth + 1);
                    } else {
                        self.value(indent + 1, String::new(), None, depth + 1);
                    }
                    self.comma();
                    n += 1;
                    if n > 100_000 {
                        return;
                    }
                }
                self.out.push(format!("{pad}{}", close as char), None);
            }
            Some(b'"') => {
                let raw = self.string().to_string();
                let value = serde_json::from_str::<String>(&raw).unwrap_or_default();
                self.out.push(format!("{pad}{prefix}{raw}"), path.map(|p| (p, value)));
            }
            Some(_) => {
                let raw = self.scalar().to_string();
                let field = path.filter(|_| raw != "null").map(|p| (p, raw.clone()));
                self.out.push(format!("{pad}{prefix}{raw}"), field);
            }
            None => {}
        }
    }
}

/// `raw` (a JSON object) pretty-printed in its own key order; `None` when it is not JSON.
pub fn pretty_json(raw: &str) -> Option<JsonView> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') || serde_json::from_str::<serde::de::IgnoredAny>(trimmed).is_err() {
        return None;
    }
    let mut p = Pretty { s: trimmed.as_bytes(), i: 0, out: JsonView::default() };
    p.value(0, String::new(), None, 0);
    Some(p.out)
}

fn span_text(line: &str, span: strata_core::Span) -> Option<&str> {
    line.get(span.start as usize..span.end as usize)
}

/// A field value without the quotes a parser may leave on it.
pub fn unquote(v: &str) -> String {
    let t = v.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        serde_json::from_str::<String>(t).unwrap_or_else(|_| t.get(1..t.len() - 1).unwrap_or_default().to_string())
    } else {
        t.to_string()
    }
}

/// The parsed fields of `line` as (name, value) pairs.
pub fn fields(line: &str, parsed: &Parsed, file: &LogFile) -> Vec<(String, String)> {
    parsed.fields.iter().filter_map(|(id, span)| Some((file.names().name(*id)?, unquote(span_text(line, *span)?)))).collect()
}

/// The selected line as JSON: a JSON line as written; any other line as its parsed parts.
pub fn as_json(line: &str, kind: FormatKind, file: &LogFile) -> JsonView {
    if kind == FormatKind::JsonLines
        && let Some(v) = pretty_json(line)
    {
        return v;
    }
    let parsed = strata_core::formats::parser(kind).parse(line.as_bytes(), file.names());
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let mut entries: Vec<(String, String, Option<String>)> = Vec::new();
    if let Some(t) = parsed.time {
        entries.push(("time".into(), q(&t.to_rfc3339()), Some(t.to_rfc3339())));
    }
    if let Some(l) = parsed.level {
        entries.push(("level".into(), q(l.as_str()), Some(l.as_str().into())));
    }
    for (name, value) in fields(line, &parsed, file) {
        let shown = if value.parse::<f64>().is_ok_and(f64::is_finite) { value.clone() } else { q(&value) };
        entries.push((name, shown, Some(value)));
    }
    let message = span_text(line, parsed.message).filter(|m| !m.is_empty()).unwrap_or(line);
    entries.push(("message".into(), q(message), None));
    let mut out = JsonView::default();
    out.push("{".into(), None);
    let n = entries.len();
    for (i, (name, shown, value)) in entries.into_iter().enumerate() {
        let comma = if i + 1 < n { "," } else { "" };
        out.push(format!("  {}: {shown}{comma}", q(&name)), value.map(|v| (name, v)));
    }
    out.push("}".into(), None);
    out
}

/// Lines looked at above a continuation line for its entry, and below an entry for its
/// continuation lines.
pub const MAX_ENTRY_LINES: u64 = 200;
/// Bytes of continuation lines shown under an entry.
const MAX_TRACE_BYTES: usize = 64 << 10;

/// Whether a parsed line continues the entry above it (a stack trace line): in timestamped text,
/// a line with neither a time nor a level.
pub fn is_continuation(kind: FormatKind, parsed: &Parsed) -> bool {
    kind == FormatKind::Timestamped && parsed.time.is_none() && parsed.level.is_none()
}

fn parse_at(file: &LogFile, kind: FormatKind, line: u64) -> Option<Parsed> {
    let l = file.line(line, 512)?;
    Some(strata_core::formats::parser(kind).parse(&l.bytes, file.names()))
}

/// The line that starts the entry `line` continues (itself when it is not a continuation line),
/// looking back at most [`MAX_ENTRY_LINES`]; `None` when no entry start is that close.
pub fn entry_start(file: &LogFile, kind: FormatKind, line: u64) -> Option<u64> {
    let mut l = line;
    loop {
        let p = parse_at(file, kind, l)?;
        if !is_continuation(kind, &p) {
            return Some(l);
        }
        if l == 0 || line - l >= MAX_ENTRY_LINES {
            return None;
        }
        l -= 1;
    }
}

/// The continuation lines right after entry `line` (a stack trace), capped.
pub fn continuation_lines(file: &LogFile, kind: FormatKind, line: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut l = line.saturating_add(1);
    while l < file.lines() && l - line <= MAX_ENTRY_LINES && bytes < MAX_TRACE_BYTES {
        let Some(raw) = file.line(l, strata_core::limits::MAX_LINE_BYTES) else { break };
        let p = strata_core::formats::parser(kind).parse(&raw.bytes, file.names());
        if !is_continuation(kind, &p) || raw.bytes.iter().all(u8::is_ascii_whitespace) {
            break;
        }
        let text = raw.text().into_owned();
        bytes += text.len();
        out.push(text);
        l += 1;
    }
    out
}

/// One row of the parts table: the name, the value as shown, and the value "Filter by" uses
/// (`None`: not a filter, like the time or the message).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub name: String,
    pub value: String,
    pub filter: Option<String>,
}

/// A line that is not JSON: as written (with its continuation lines) and its parsed parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartsView {
    /// The line as written, then the continuation lines of its entry, one per line.
    pub raw: String,
    /// How many continuation lines `raw` holds.
    pub trace: usize,
    pub parts: Vec<Part>,
    /// The entry (line number, 0-based) this continuation line belongs to.
    pub continues: Option<u64>,
}

/// `line`'s parsed parts: time, level, the service, the message, then the other fields.
pub fn parts(line: &str, kind: FormatKind, file: &LogFile) -> Vec<Part> {
    let parsed = strata_core::formats::parser(kind).parse(line.as_bytes(), file.names());
    let mut out = Vec::new();
    if let Some(t) = parsed.time {
        out.push(Part { name: "time".into(), value: t.to_rfc3339(), filter: None });
    }
    if let Some(l) = parsed.level {
        out.push(Part { name: "level".into(), value: l.as_str().into(), filter: Some(l.as_str().into()) });
    }
    let fields = fields(line, &parsed, file);
    let service = fields.iter().position(|(n, _)| super::table::is_service(n));
    if let Some((n, v)) = service.and_then(|i| fields.get(i)) {
        out.push(Part { name: n.clone(), value: v.clone(), filter: Some(v.clone()) });
    }
    let message = span_text(line, parsed.message).filter(|m| !m.trim().is_empty()).unwrap_or(line);
    out.push(Part { name: "message".into(), value: message.replace('\t', "    "), filter: None });
    for (i, (n, v)) in fields.into_iter().enumerate() {
        if Some(i) != service {
            out.push(Part { name: n, value: v.clone(), filter: Some(v) });
        }
    }
    out
}

/// What the pane keeps between frames.
#[derive(Default)]
pub struct DetailState {
    key: Option<(u64, u64, usize, FormatKind)>,
    view: JsonView,
    /// Set for a line that is not JSON (the pane shows it instead of `view`).
    plain: Option<PartsView>,
    header: String,
    title: String,
    revision: u64,
    /// The output line picked for "Filter by".
    picked: Option<usize>,
}

impl DetailState {
    fn refresh(&mut self, file: &Arc<LogFile>, kind: FormatKind, line: u64) {
        let key = (file.id().0, line, Arc::as_ptr(file) as usize, kind);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.revision = self.revision.wrapping_add(1);
        let Some(l) = file.line(line, strata_core::limits::MAX_LINE_BYTES) else {
            self.view = JsonView::default();
            self.title = format!("Line {}", fmt::thousands(line + 1));
            self.header = "not indexed yet".into();
            return;
        };
        let text = l.text();
        let json = (kind == FormatKind::JsonLines).then(|| pretty_json(&text)).flatten();
        let parsed = strata_core::formats::parser(kind).parse(&l.bytes, file.names());
        let mut parts = Vec::new();
        if let Some(t) = parsed.time {
            parts.push(fmt::time_of_day(t));
        }
        if let Some(lv) = parsed.level {
            parts.push(lv.as_str().to_string());
        }
        let fields = fields(&text, &parsed, file);
        if let Some((_, svc)) = fields.iter().find(|(n, _)| super::table::is_service(n)) {
            parts.push(svc.clone());
        }
        if l.cut {
            parts.push(format!("cut at {}", fmt::bytes(l.bytes.len() as u64)));
        }
        self.title = format!("Line {}", fmt::thousands(line + 1));
        self.picked = None;
        if let Some(view) = json {
            self.view = view;
            self.plain = None;
            self.picked =
                self.view.fields.iter().position(|f| f.as_ref().is_some_and(|(n, _)| super::table::is_service(n))).or_else(|| {
                    self.view.fields.iter().position(|f| f.as_ref().is_some_and(|(n, _)| !matches!(n.as_str(), "time" | "ts" | "level")))
                });
        } else {
            self.view = JsonView::default();
            let mut view = PartsView { raw: text.replace('\t', "    "), ..PartsView::default() };
            if is_continuation(kind, &parsed) {
                view.continues = entry_start(file, kind, line).filter(|&e| e != line);
                if let Some(e) = view.continues {
                    parts.push(format!("continues line {}", fmt::thousands(e + 1)));
                }
            } else {
                view.parts = self::parts(&text, kind, file);
                for t in continuation_lines(file, kind, line) {
                    view.raw.push('\n');
                    view.raw.push_str(&t.replace('\t', "    "));
                    view.trace += 1;
                }
                if view.trace > 0 {
                    parts.push(format!("{} more {}", view.trace, if view.trace == 1 { "line" } else { "lines" }));
                }
            }
            self.picked = view
                .parts
                .iter()
                .position(|p| p.filter.is_some() && super::table::is_service(&p.name))
                .or_else(|| view.parts.iter().position(|p| p.filter.is_some() && p.name != "level"));
            self.plain = Some(view);
        }
        self.header = parts.join(" · ");
    }

    /// The (field, value) "Filter by" uses.
    fn picked_filter(&self) -> Option<(String, String)> {
        let i = self.picked?;
        match &self.plain {
            Some(v) => v.parts.get(i).and_then(|p| Some((p.name.clone(), p.filter.clone()?))),
            None => self.view.fields.get(i).cloned().flatten(),
        }
    }
}

/// Draw the pane for line `line` of `file`, or a hint when nothing is selected.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, st: &mut DetailState, file: &Arc<LogFile>, kind: FormatKind, line: Option<u64>) {
    let t = cx.tokens();
    let Some(line) = line else {
        ui.add_space(t.space.lg);
        ui.vertical_centered(|ui| {
            let r = ui.label(egui::RichText::new("Select a line to see it here").color(t.text.secondary));
            automation::register(ui, &r, "strata.detail.empty", "Select a line to see it here", Role::Label);
        });
        return;
    };
    st.refresh(file, kind, line);
    let header_h = 36.0;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), header_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add_space(t.space.xl);
        let title = ui.label(egui::RichText::new(&st.title).strong().color(t.text.primary));
        automation::register(ui, &title, "strata.detail.title", &st.title, Role::Label);
        let header = ui.label(egui::RichText::new(&st.header).color(t.text.secondary));
        automation::register(ui, &header, "strata.detail.summary", &st.header, Role::Label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(t.space.xl);
            if let Some((field, value)) = st.picked_filter() {
                let label = format!("Filter by {field}");
                if Button::new("strata.detail.filter", &label).size(Size::Sm).show(ui).on_hover_text(format!("{field} = {value}")).clicked() {
                    cx.dispatch(ids::DETAIL_FILTER_BY_VALUE, json!({"file": file.id().0, "field": field, "value": value}));
                }
            }
            if st.plain.is_none() && Button::new("strata.detail.copy_json", "Copy JSON").size(Size::Sm).show(ui).clicked() {
                cx.dispatch(ids::DETAIL_COPY_JSON, json!({}));
            }
            if Button::new("strata.detail.copy_line", "Copy line").size(Size::Sm).variant(Variant::Secondary).show(ui).clicked() {
                cx.dispatch(ids::DETAIL_COPY_LINE, json!({}));
            }
        });
    });
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), ui.cursor().top(), egui::Stroke::new(1.0, t.separator));
    ui.add_space(t.space.sm);
    if st.plain.is_some() {
        plain_body(ui, &t, st);
        return;
    }
    let marks: Vec<Range<usize>> = st.picked.and_then(|i| st.view.line_range(i)).filter(|r| !r.is_empty()).into_iter().collect();
    egui::Frame::new().inner_margin(egui::Margin { left: t.space.xl as i8, right: t.space.md as i8, top: 0, bottom: 0 }).show(ui, |ui| {
        let out =
            CodeView::new("strata.detail.json", &st.view.text).lang(Some("json")).revision(st.revision).line_numbers(false).marks(&marks).show(ui);
        if let Some(l) = out.clicked_line
            && st.view.fields.get(l).is_some_and(Option::is_some)
        {
            st.picked = Some(l);
        }
    });
}

/// A line that is not JSON: the raw line (and its stack trace) on the left, the parts table on
/// the right (stacked when the pane is narrow). Both stay inside the pane's height.
fn plain_body(ui: &mut egui::Ui, t: &ashlar::ui::tokens::Tokens, st: &mut DetailState) {
    let Some(view) = &st.plain else { return };
    let mono = egui::FontId::monospace(t.type_scale.mono);
    let row_h = ui.fonts_mut(|f| f.row_height(&mono)) + 4.0;
    let avail = ui.available_rect_before_wrap();
    ui.allocate_rect(avail, egui::Sense::hover());
    let inner = egui::Rect::from_min_max(
        egui::pos2(avail.left() + t.space.xl, avail.top()),
        egui::pos2(avail.right() - t.space.md, avail.bottom() - t.space.sm),
    );
    if inner.width() < 40.0 || inner.height() < 20.0 {
        return;
    }
    let gap = t.space.lg;
    let (raw_rect, side_rect) = if inner.width() >= 560.0 {
        // the parts want room for a full time (`2026-11-12T00:00:00.742Z`) beside its name
        let side_w = (inner.width() * 0.42).clamp(300.0, 440.0);
        let split = (inner.right() - side_w - gap).round();
        (
            egui::Rect::from_min_max(inner.min, egui::pos2(split, inner.bottom())),
            egui::Rect::from_min_max(egui::pos2(split + gap, inner.top()), inner.max),
        )
    } else {
        let split = inner.top() + (inner.height() * 0.45).round();
        (
            egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), split)),
            egui::Rect::from_min_max(egui::pos2(inner.left(), split + gap), inner.max),
        )
    };
    // the raw line, as written, with its continuation lines
    let mut raw_ui = ui.new_child(egui::UiBuilder::new().max_rect(raw_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let pad = t.space.sm;
    egui::Frame::new().fill(t.surface.sunken).corner_radius(t.radius.sm).inner_margin(egui::Margin::same(pad as i8)).show(&mut raw_ui, |ui| {
        ui.set_width(ui.available_width());
        let lines = view.raw.lines().count().max(1) as f32;
        let h = (lines * row_h).min(raw_rect.height() - pad * 2.0).max(row_h);
        CodeView::new("strata.detail.raw", &view.raw).lang(None).revision(st.revision).line_numbers(false).wrap(true).max_height(h).show(ui);
    });
    // the parts (or, for a continuation line, the entry it belongs to)
    let mut side = ui.new_child(egui::UiBuilder::new().max_rect(side_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut side;
    if let Some(e) = view.continues {
        let note = format!("Part of the entry that starts at line {} (a stack trace or another multi-line message).", fmt::thousands(e + 1));
        let r = ui.add(egui::Label::new(egui::RichText::new(&note).color(t.text.secondary)).wrap());
        automation::register(ui, &r, "strata.detail.continues", &note, Role::Label);
        return;
    }
    if view.parts.is_empty() {
        return;
    }
    let name_w = view.parts.iter().map(|p| p.name.chars().count()).max().unwrap_or(4).clamp(4, 16) as f32 * (t.type_scale.mono * 0.62) + t.space.lg;
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt("strata.detail.parts").auto_shrink([false, false]).max_height(side_rect.height()).show(ui, |ui| {
        for (i, p) in view.parts.iter().enumerate() {
            let top = ui.cursor().top();
            let full = ui.available_width();
            let bg = ui.painter().add(egui::Shape::Noop);
            let resp = ui
                .horizontal_top(|ui| {
                    ui.add_space(t.space.xs);
                    ui.allocate_ui_with_layout(egui::vec2(name_w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_size(egui::vec2(name_w, row_h));
                        ui.add(
                            egui::Label::new(egui::RichText::new(&p.name).font(mono.clone()).color(t.text.secondary)).selectable(false).truncate(),
                        );
                    });
                    let color = if p.name == "level" { super::theme::level_color(t, strata_core::Level::parse(&p.value)) } else { t.text.primary };
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width() - t.space.xs, row_h),
                        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                        |ui| {
                            ui.add(egui::Label::new(egui::RichText::new(&p.value).font(mono.clone()).color(color)).selectable(false).wrap());
                        },
                    );
                })
                .response;
            let row =
                egui::Rect::from_min_max(egui::pos2(resp.rect.left(), top), egui::pos2(resp.rect.left() + full, resp.rect.bottom().max(top + row_h)));
            let id = format!("strata.detail.part.{}", p.name);
            let sense = if p.filter.is_some() { egui::Sense::click() } else { egui::Sense::hover() };
            let hit = ui.interact(row, ui.make_persistent_id(&id), sense);
            let fill = if st.picked == Some(i) {
                Some(super::theme::tint(t.accent, 34))
            } else if hit.hovered() && p.filter.is_some() {
                Some(super::theme::tint(t.text.primary, 12))
            } else {
                None
            };
            if let Some(fill) = fill {
                ui.painter().set(bg, egui::Shape::rect_filled(row, t.radius.sm, fill));
            }
            automation::register(ui, &hit, &id, &format!("{}: {}", p.name, p.value), Role::Label);
            if hit.clicked() {
                clicked = Some(i);
            }
        }
    });
    if clicked.is_some() {
        st.picked = clicked;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_keeps_its_order_and_names_its_fields() {
        let v = pretty_json(r#"{"ts":"2026-11-12T00:00:00.014Z","level":"info","http":{"status":502,"path":"/v1"},"tags":[],"n":null}"#)
            .unwrap_or_default();
        let lines: Vec<&str> = v.text.lines().collect();
        assert_eq!(lines[0], "{");
        assert_eq!(lines[1], r#"  "ts": "2026-11-12T00:00:00.014Z","#);
        assert_eq!(lines[3], r#"  "http": {"#);
        assert_eq!(lines[4], r#"    "status": 502,"#);
        assert_eq!(lines[7], r#"  "tags": [],"#);
        assert_eq!(lines[8], r#"  "n": null"#);
        assert_eq!(lines[9], "}");
        assert_eq!(v.fields[2], Some(("level".into(), "info".into())));
        assert_eq!(v.fields[4], Some(("http.status".into(), "502".into())));
        assert_eq!(v.fields[8], None);
        assert_eq!(v.fields.len(), lines.len());
        assert_eq!(v.line_range(1).and_then(|r| v.text.get(r)), Some(lines[1]));
    }

    #[test]
    fn hostile_text_is_not_json() {
        for s in ["", "{", "plain text", "{\"a\":", "[1,2]", "{\"a\":\"\\u00\"}"] {
            assert!(pretty_json(s).is_none(), "{s}");
        }
        let deep = format!("{}{}", "{\"a\":".repeat(100), "1".to_string() + &"}".repeat(100));
        let v = pretty_json(&deep);
        assert!(v.is_none_or(|v| !v.text.is_empty()));
    }

    #[test]
    fn values_lose_their_quotes() {
        assert_eq!(unquote("\"api\""), "api");
        assert_eq!(unquote("502"), "502");
        assert_eq!(unquote("\"a\\\"b\""), "a\"b");
    }
}
