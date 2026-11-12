//! The toolbar (44 pt): search field with regex / case / whole-word toggles and the match
//! counter, level chips with counts, filter chips, the filter field, the time range and Follow.

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Details, Role};
use ashlar::ui::search::SearchField;
use ashlar::ui::tokens::Tokens;
use strata_core::{Level, LevelCounts};

use super::fmt;
use super::theme;
use crate::commands::ids;
use crate::doc::{Actor, OpenFile, StrataDoc, view_keys};

/// What the toolbar keeps between frames.
#[derive(Default)]
pub struct ToolbarState {
    pub query: String,
    pub regex: bool,
    pub case: bool,
    pub word: bool,
    pub filter: String,
    /// The search last sent, to tell Enter (next match) from a new search.
    sent: Option<(String, bool, bool, bool)>,
}

/// The search counter.
pub struct Counter {
    /// 1-based index of the selected match, when the selection is a match.
    pub current: Option<u64>,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Paint a pill-shaped toggle chip. Returns its response.
fn chip(ui: &mut egui::Ui, t: &Tokens, id: &str, text: &str, count: Option<&str>, color: egui::Color32, on: bool, mono: bool) -> egui::Response {
    let font = if mono { egui::FontId::monospace(t.type_scale.mono) } else { egui::FontId::proportional(12.0) };
    let fg = if on { color } else { t.text.secondary };
    let mut job = egui::text::LayoutJob::default();
    job.append(text, 0.0, egui::TextFormat { font_id: font.clone(), color: fg, ..Default::default() });
    if let Some(c) = count {
        job.append(c, 6.0, egui::TextFormat { font_id: font, color: if on { t.text.primary } else { t.text.secondary }, ..Default::default() });
    }
    let galley = ui.painter().layout_job(job);
    let size = egui::vec2(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let stroke = if on { color } else { t.border };
    let fill = if on {
        theme::tint(color, 30)
    } else if resp.hovered() {
        t.surface.hover
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect(rect, 14.0, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    ui.painter().galley(egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), 16.0, egui::Stroke::new(t.stroke.focus, t.focus), egui::StrokeKind::Outside);
    }
    let label = match count {
        Some(c) => format!("{text} {c}"),
        None => text.to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &label));
    automation::register_with(ui, &resp, id, &label, Role::Toggle, Details { value: count, selected: Some(on) });
    resp
}

/// A small square toggle (`.*`, `Aa`, `W`).
fn toggle(ui: &mut egui::Ui, t: &Tokens, id: &str, text: &str, name: &str, on: &mut bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let fill = if *on {
        theme::tint(t.accent, 40)
    } else if resp.hovered() {
        t.surface.hover
    } else {
        t.surface.raised
    };
    let stroke = if *on { t.accent } else { t.border };
    ui.painter().rect(rect, t.radius.md, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let color = if *on { t.accent } else { t.text.secondary };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::monospace(12.0), color);
    let resp = resp.on_hover_text(name);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, name));
    automation::register_with(ui, &resp, id, name, Role::Toggle, Details { value: None, selected: Some(*on) });
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

fn separator(ui: &mut egui::Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 22.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.border);
}

/// Draw the toolbar for `f` (or a bare one with no file).
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, st: &mut ToolbarState, f: Option<&OpenFile>, levels: &LevelCounts, counter: &Counter) {
    let t = cx.tokens();
    let focus = cx.view::<String>(view_keys::FOCUS);
    // the search in view state wins over what was typed (an agent or a script ran search.run)
    let current = cx.session().view_state().get(view_keys::SEARCH).cloned();
    let cur_query = current.as_ref().and_then(|v| v.get("query")).and_then(Value::as_str).unwrap_or_default().to_string();
    let flag = |k: &str| current.as_ref().and_then(|v| v.get(k)).and_then(Value::as_bool).unwrap_or(false);
    let in_view = (cur_query.clone(), flag("regex"), flag("case"), flag("whole_word"));
    if st.sent.as_ref() != Some(&in_view) {
        st.query.clone_from(&in_view.0);
        (st.regex, st.case, st.word) = (in_view.1, in_view.2, in_view.3);
        st.sent = Some(in_view);
    }
    let file_id = f.map(|f| f.id.0);
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = t.space.md;
        ui.add_space(t.space.lg);
        // search
        let search = SearchField::new("strata.toolbar.search", &mut st.query)
            .hint(&format!("Search  {}", fmt::keys("⌘F")))
            .label("Search")
            .width(260.0)
            .height(30.0)
            .show(ui);
        if focus.as_deref() == Some("search") {
            SearchField::focus(ui, "strata.toolbar.search");
            cx.set_view(view_keys::FOCUS, &Value::Null);
        }
        let mut rerun = search.submitted || search.cleared;
        rerun |= toggle(ui, &t, "strata.toolbar.regex", ".*", "Regular expression", &mut st.regex);
        rerun |= toggle(ui, &t, "strata.toolbar.case", "Aa", "Match case", &mut st.case);
        rerun |= toggle(ui, &t, "strata.toolbar.word", "W", "Whole word", &mut st.word);
        if rerun && let Some(file) = file_id {
            let want = (st.query.clone(), st.regex, st.case, st.word);
            let same = st.sent.as_ref() == Some(&want);
            if st.query.is_empty() {
                cx.dispatch(ids::SEARCH_CLEAR, json!({}));
            } else if same && search.submitted {
                let back = ui.input(|i| i.modifiers.shift);
                cx.dispatch(if back { ids::SEARCH_PREV } else { ids::SEARCH_NEXT }, json!({"file": file}));
                SearchField::focus(ui, "strata.toolbar.search");
            } else {
                cx.dispatch(ids::SEARCH_RUN, json!({"file": file, "query": st.query, "regex": st.regex, "case": st.case, "whole_word": st.word}));
                SearchField::focus(ui, "strata.toolbar.search");
            }
        }
        if !cur_query.is_empty() {
            let text = match (&counter.error, counter.current, counter.done) {
                (Some(_), ..) => "bad query".to_string(),
                (None, _, _) if counter.total == 0 && counter.done => "no matches".to_string(),
                (None, Some(i), _) => format!("{} of {}", fmt::thousands(i), fmt::thousands(counter.total)),
                (None, None, true) => format!("{} matches", fmt::thousands(counter.total)),
                (None, None, false) => format!("{} so far…", fmt::thousands(counter.total)),
            };
            let r = ui.label(egui::RichText::new(&text).size(12.0).color(t.text.secondary));
            if let Some(e) = &counter.error {
                r.clone().on_hover_text(e);
            }
            automation::register(ui, &r, "strata.toolbar.matches", &text, Role::Status);
        }
        let Some(f) = f else { return };
        separator(ui, &t);
        // level chips
        let groups: [(&str, &str, &[Level]); 4] = [
            ("error", "Error", &[Level::Error, Level::Fatal]),
            ("warn", "Warn", &[Level::Warn]),
            ("info", "Info", &[Level::Info]),
            ("debug", "Debug", &[Level::Debug, Level::Trace]),
        ];
        for (key, name, members) in groups {
            let count: u64 = members.iter().map(|l| levels.get(*l)).sum();
            let shown = members.iter().any(|l| !f.hidden_levels.contains(l));
            let color = theme::level_color(&t, members.first().copied());
            let count_text = fmt::short_count(count);
            if chip(ui, &t, &format!("strata.toolbar.level.{key}"), name, Some(&count_text), color, shown, false).clicked() {
                for l in members {
                    cx.dispatch(ids::FILTER_LEVEL, json!({"file": f.id.0, "level": l.as_str(), "on": !shown}));
                }
            }
        }
        // filter chips
        if !f.filters.is_empty() || f.time_from.is_some() || f.time_to.is_some() {
            separator(ui, &t);
        }
        for c in &f.filters {
            let ai = if c.by == Actor::Agent { " · AI" } else { "" };
            let text = format!("{}{ai}", c.text);
            let r = chip(ui, &t, &format!("strata.toolbar.filter.{}", c.id), &text, None, t.text.primary, c.enabled, true);
            if r.clicked() {
                cx.dispatch(ids::FILTER_TOGGLE, json!({"file": f.id.0, "id": c.id}));
            }
            let x = ui.add(egui::Button::new(egui::RichText::new("×").color(t.text.secondary)).frame(false)).on_hover_text("Remove filter");
            automation::register(ui, &x, &format!("strata.toolbar.filter.{}.remove", c.id), "Remove filter", Role::Button);
            if x.clicked() {
                cx.dispatch(ids::FILTER_REMOVE, json!({"file": f.id.0, "id": c.id}));
            }
        }
        if f.time_from.is_some() || f.time_to.is_some() {
            let from = f.time_from.map(fmt::hms).unwrap_or_else(|| "start".into());
            let to = f.time_to.map(fmt::hms).unwrap_or_else(|| "end".into());
            let text = format!("{from} – {to}");
            chip(ui, &t, "strata.toolbar.time_range", &text, None, t.text.primary, true, true);
            let x = ui.add(egui::Button::new(egui::RichText::new("×").color(t.text.secondary)).frame(false)).on_hover_text("Remove the time range");
            automation::register(ui, &x, "strata.toolbar.time_range.remove", "Remove the time range", Role::Button);
            if x.clicked() {
                cx.dispatch(ids::FILTER_TIME_RANGE, json!({"file": f.id.0}));
            }
        }
        // the filter field
        let edit_id = ui.make_persistent_id("strata.toolbar.filter_field");
        let fr = ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .id(edit_id)
                .hint_text(format!("Filter  {}", fmt::keys("⌘L")))
                .font(egui::FontId::monospace(t.type_scale.mono))
                .desired_width(170.0)
                .margin(egui::Margin::symmetric(8, 6)),
        );
        automation::register(ui, &fr, "strata.toolbar.filter_field", "Filter", Role::Field);
        if focus.as_deref() == Some("filter") {
            fr.request_focus();
            cx.set_view(view_keys::FOCUS, &Value::Null);
        }
        if fr.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.filter.trim().is_empty() {
            cx.dispatch(ids::VIEW_FILTER, json!({"file": f.id.0, "filter": st.filter.trim()}));
            st.filter.clear();
        }
        // follow, on the right
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(t.space.lg);
            let label = if f.follow { "● Following" } else { "Follow" };
            let color = if f.follow { t.accent } else { t.text.primary };
            if chip(ui, &t, "strata.toolbar.follow", label, None, color, f.follow, false).on_hover_text("Follow the file as it grows (F)").clicked() {
                cx.dispatch(ids::VIEW_FOLLOW, json!({"file": f.id.0}));
            }
        });
    });
}
