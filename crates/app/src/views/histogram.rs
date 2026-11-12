//! The histogram strip (56 pt) above the table: lines per time bucket, errors stacked in red and
//! warnings in yellow, the visible range drawn as a bracket. Drag across it to limit the table
//! to that time range (`filter.time_range`); hover shows a bucket's time and counts. Nothing
//! animates.

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use strata_core::{Histogram, Level, Timestamp};

use super::{fmt, theme};
use crate::commands::ids;
use crate::doc::StrataDoc;

/// What the strip keeps between frames.
#[derive(Default)]
pub struct HistState {
    drag_from: Option<f32>,
}

/// Draw the strip. `visible`: the times of the first and last visible rows.
pub fn show(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_, StrataDoc>,
    st: &mut HistState,
    file: u64,
    hist: Option<&Histogram>,
    visible: Option<(Timestamp, Timestamp)>,
) {
    let t = cx.tokens();
    let full = ui.available_rect_before_wrap();
    let (outer, resp) = ui.allocate_exact_size(full.size(), egui::Sense::click_and_drag());
    let rect = outer.shrink2(egui::vec2(t.space.xl, 0.0));
    let axis_h = 12.0;
    let bars = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 8.0), egui::pos2(rect.right(), rect.bottom() - axis_h - 4.0));
    let p = ui.painter();
    p.hline(outer.x_range(), outer.bottom() - 0.5, egui::Stroke::new(1.0, t.separator));
    let Some(h) = hist.filter(|h| !h.buckets.is_empty()) else {
        let text = if hist.is_some() { "No timestamps to chart" } else { "" };
        p.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(11.0), t.text.secondary);
        automation::register(ui, &resp, "strata.histogram", "Lines over time: no data", Role::Other);
        return;
    };
    let n = h.buckets.len();
    let max = h.buckets.iter().map(|b| b.counts.total()).max().unwrap_or(1).max(1) as f32;
    let w = bars.width() / n as f32;
    let gap = if w > 4.0 { 2.0 } else { 0.0 };
    let base = if t.dark { egui::Color32::from_rgb(0x25, 0x2B, 0x34) } else { t.border };
    let start = h.buckets.first().map_or(0, |b| b.start.0);
    let end = h.buckets.last().map_or(0, |b| b.start.0.saturating_add(h.width));
    let x_of = |time: i64| bars.left() + bars.width() * ((time - start) as f32 / (end - start).max(1) as f32);
    let time_of = |x: f32| start.saturating_add(((x - bars.left()) / bars.width().max(1.0) * (end - start) as f32) as i64);
    // the visible range
    if let Some((a, b)) = visible {
        let (xa, xb) = (x_of(a.0).clamp(bars.left(), bars.right()), x_of(b.0.saturating_add(1)).clamp(bars.left(), bars.right()));
        p.rect_filled(egui::Rect::from_x_y_ranges(xa..=xb.max(xa + 2.0), bars.y_range()), 2.0, theme::tint(t.text.primary, 10));
        let s = egui::Stroke::new(1.0, t.text.secondary);
        p.vline(xa, bars.y_range(), s);
        p.vline(xb.max(xa + 2.0), bars.y_range(), s);
    }
    for (i, b) in h.buckets.iter().enumerate() {
        let total = b.counts.total() as f32;
        if total <= 0.0 {
            continue;
        }
        let x0 = bars.left() + i as f32 * w;
        let col = egui::Rect::from_min_max(
            egui::pos2(x0, bars.bottom() - (total / max * bars.height()).max(2.0)),
            egui::pos2(x0 + (w - gap).max(1.0), bars.bottom()),
        );
        p.rect_filled(col, 1.0, base);
        let err = (b.counts.get(Level::Error) + b.counts.get(Level::Fatal)) as f32;
        let warn = b.counts.get(Level::Warn) as f32;
        let eh = err / max * bars.height();
        let wh = warn / max * bars.height();
        if warn > 0.0 {
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(col.left(), col.bottom() - eh - wh), egui::pos2(col.right(), col.bottom() - eh)),
                0.0,
                t.status.warning,
            );
        }
        if err > 0.0 {
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(col.left(), col.bottom() - eh.max(1.5)), col.right_bottom()), 0.0, t.status.danger);
        }
    }
    // the axis: five times
    for k in 0..5 {
        let time = start + (end - start) / 4 * k;
        let x = x_of(time);
        let align = match k {
            0 => egui::Align2::LEFT_BOTTOM,
            4 => egui::Align2::RIGHT_BOTTOM,
            _ => egui::Align2::CENTER_BOTTOM,
        };
        let label = if end - start < 120 * Timestamp::MICROS_PER_SECOND { fmt::hms(Timestamp(time)) } else { fmt::hour_minute(Timestamp(time)) };
        p.text(egui::pos2(x, rect.bottom() - 2.0), align, label, egui::FontId::monospace(10.0), t.text.secondary);
    }
    // drag to choose a time range
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        st.drag_from = Some(pos.x);
    }
    if let (Some(a), Some(pos)) = (st.drag_from, resp.interact_pointer_pos().or(resp.hover_pos()))
        && resp.dragged()
    {
        let (x0, x1) = (a.min(pos.x), a.max(pos.x));
        p.rect(
            egui::Rect::from_x_y_ranges(x0..=x1, bars.y_range()),
            2.0,
            theme::tint(t.accent, 40),
            egui::Stroke::new(1.0, t.accent),
            egui::StrokeKind::Inside,
        );
    }
    if resp.drag_stopped()
        && let (Some(a), Some(pos)) = (st.drag_from.take(), resp.interact_pointer_pos())
        && (pos.x - a).abs() > 3.0
    {
        let (x0, x1) = (a.min(pos.x).max(bars.left()), a.max(pos.x).min(bars.right()));
        let from = Timestamp(time_of(x0)).to_rfc3339();
        let to = Timestamp(time_of(x1)).to_rfc3339();
        cx.dispatch(ids::FILTER_TIME_RANGE, json!({"file": file, "from": from, "to": to}));
    }
    // hover: the bucket's time and counts
    if let Some(pos) = resp.hover_pos()
        && st.drag_from.is_none()
        && bars.x_range().contains(pos.x)
    {
        let i = (((pos.x - bars.left()) / w) as usize).min(n - 1);
        if let Some(b) = h.buckets.get(i) {
            let c = &b.counts;
            let tip = format!(
                "{} · {} lines · {} errors · {} warnings",
                fmt::time_of_day(b.start),
                fmt::thousands(c.total()),
                fmt::thousands(c.get(Level::Error) + c.get(Level::Fatal)),
                fmt::thousands(c.get(Level::Warn))
            );
            resp.clone().on_hover_text_at_pointer(tip);
        }
    }
    let total: u64 = h.buckets.iter().map(|b| b.counts.total()).sum();
    let label = format!("Lines over time: {} buckets, {} lines; drag to choose a time range", n, fmt::thousands(total));
    automation::register(ui, &resp, "strata.histogram", &label, Role::Other);
}
