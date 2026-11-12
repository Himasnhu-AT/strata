//! Screens and states around the table: the open / empty screen, the indexing bar, a file that
//! could not be read, the "file was replaced" banner and the "Unreadable" dialog.

use ashlar::fs::RecentFiles;
use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::content::Banner;
use ashlar::ui::controls::Tone;
use ashlar::ui::controls::{Button, Size, Variant};
use ashlar::ui::dialog::{Dialog, DialogButton, DialogOutcome};

use super::sidebar::{RECENT, RECENT_MAX};
use super::{fmt, theme};
use crate::commands::ids;
use crate::doc::StrataDoc;

fn label(ui: &mut egui::Ui, id: &str, text: &str, rich: egui::RichText) -> egui::Response {
    let r = ui.label(rich);
    automation::register(ui, &r, id, text, Role::Label);
    r
}

/// Screen 1, "Open / empty": a drop target, "Open a file", recent files and a line on the agent.
pub fn empty(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    let t = cx.tokens();
    let recent: Vec<std::path::PathBuf> = RecentFiles::load(cx.settings, RECENT, RECENT_MAX).paths().to_vec();
    let width = ui.available_width().min(480.0);
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let top = ((ui.available_height() - 420.0) / 2.0).max(t.space.xl);
    ui.add_space(top);
    ui.vertical_centered(|ui| {
        ui.set_max_width(width);
        // the drop target
        let frame = egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, if hovering { t.accent } else { t.border_strong }))
            .fill(if hovering { theme::tint(t.accent, 16) } else { egui::Color32::TRANSPARENT })
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(24, 36));
        frame.show(ui, |ui| {
            ui.set_width(width - 48.0);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                let p = ui.painter();
                let s = egui::Stroke::new(1.5, t.accent);
                for (i, w) in [16.0, 16.0, 10.0, 7.0].into_iter().enumerate() {
                    let y = r.top() + 8.0 + i as f32 * 6.5;
                    p.line_segment([egui::pos2(r.left() + 6.0, y), egui::pos2(r.left() + 6.0 + w * 1.6, y)], s);
                }
                let ax = r.right() - 10.0;
                p.line_segment([egui::pos2(ax, r.top() + 22.0), egui::pos2(ax, r.bottom() - 4.0)], s);
                p.line_segment([egui::pos2(ax - 5.0, r.bottom() - 9.0), egui::pos2(ax, r.bottom() - 4.0)], s);
                p.line_segment([egui::pos2(ax + 5.0, r.bottom() - 9.0), egui::pos2(ax, r.bottom() - 4.0)], s);
                ui.add_space(t.space.md);
                label(
                    ui,
                    "strata.empty.title",
                    "Drop a log file here",
                    egui::RichText::new("Drop a log file here").size(t.type_scale.title).color(t.text.primary),
                );
                ui.add_space(t.space.sm);
                let body = "Any size. JSON lines, logfmt, syslog and plain text are detected automatically. .gz works too.";
                label(ui, "strata.empty.body", body, egui::RichText::new(body).color(t.text.secondary));
                ui.add_space(t.space.lg);
                if Button::new("strata.empty.open", "Open a file…  ⌘⇧O").variant(Variant::Primary).size(Size::Lg).show(ui).clicked() {
                    cx.dispatch(ids::LOG_OPEN_DIALOG, json!({}));
                }
            });
        });
        ui.add_space(t.space.xl);
        // recent files
        if !recent.is_empty() {
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                label(ui, "strata.empty.recent", "Recent", egui::RichText::new("RECENT").size(11.0).strong().color(t.text.secondary));
                for (i, p) in recent.iter().enumerate() {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dir = p.parent().map(fmt::tilde).unwrap_or_default();
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &name,
                        0.0,
                        egui::TextFormat { font_id: egui::FontId::proportional(13.0), color: t.text.primary, ..Default::default() },
                    );
                    job.append(
                        &dir,
                        8.0,
                        egui::TextFormat { font_id: egui::FontId::proportional(13.0), color: t.text.secondary, ..Default::default() },
                    );
                    let r = ui.add(egui::Button::new(job).frame(false).min_size(egui::vec2(width, 30.0)));
                    automation::register(ui, &r, &format!("strata.empty.recent.{i}"), &name, Role::Link);
                    if r.clicked() {
                        cx.dispatch(ids::LOG_OPEN_DIALOG, json!({"path": p.display().to_string()}));
                    }
                }
            });
            ui.add_space(t.space.lg);
        }
        // the agent
        egui::Frame::new()
            .fill(t.surface.panel)
            .stroke(egui::Stroke::new(1.0, t.border))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(width - 28.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Let Claude, Cursor or another agent read logs with you.").color(t.text.primary));
                    let link = ui.add(egui::Button::new(egui::RichText::new("Connect an AI agent in Settings ▸ AI").color(t.accent)).frame(false));
                    automation::register(ui, &link, "strata.empty.agent", "Connect an AI agent in Settings ▸ AI", Role::Link);
                    if link.clicked() {
                        cx.dispatch(ids::SETTINGS_OPEN, json!({"section": "ai"}));
                    }
                });
            });
    });
}

/// The bar above the table while a file indexes: progress, what can be done meanwhile, Stop.
pub fn indexing_bar(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, file: u64, frac: f32, done: u64, total: u64, lines: u64) {
    let t = cx.tokens();
    egui::Frame::new().fill(theme::tint(t.accent, 18)).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let pct = format!("Indexing {:.0} %", (frac * 100.0).floor().clamp(0.0, 100.0));
            label(ui, "strata.indexing.title", &pct, egui::RichText::new(&pct).strong().color(t.text.primary));
            let more = format!(
                "{} of {} · {} lines so far · you can scroll and search the indexed part",
                fmt::bytes(done),
                fmt::bytes(total),
                fmt::thousands(lines)
            );
            label(ui, "strata.indexing.detail", &more, egui::RichText::new(&more).color(t.text.secondary));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new("strata.indexing.stop", "Stop  Esc").size(Size::Sm).show(ui).clicked() {
                    cx.dispatch(ids::LOG_STOP_INDEXING, json!({"file": file}));
                }
            });
        });
        ui.add_space(6.0);
        let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
        ui.painter().rect_filled(r, 2.0, t.border);
        ui.painter().rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(r.width() * frac.clamp(0.0, 1.0), 4.0)), 2.0, t.accent);
        automation::register_with(
            ui,
            &resp,
            "strata.indexing.progress",
            "Indexing progress",
            Role::Progress,
            automation::Details { value: Some(&format!("{:.0}", frac * 100.0)), selected: None },
        );
    });
}

/// A file that could not be opened (restored workspace, reload): the reason, Retry and Close.
pub fn load_error(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, file: u64, path: &std::path::Path, reason: &str) {
    let t = cx.tokens();
    ui.add_space((ui.available_height() * 0.3).max(t.space.xl));
    ui.vertical_centered(|ui| {
        label(
            ui,
            "strata.file_error.title",
            "This file can't be read",
            egui::RichText::new("This file can't be read").size(t.type_scale.heading).color(t.text.primary),
        );
        ui.add_space(t.space.sm);
        label(ui, "strata.file_error.reason", reason, egui::RichText::new(reason).color(t.status.danger));
        let p = fmt::tilde(path);
        label(ui, "strata.file_error.path", &p, egui::RichText::new(&p).monospace().color(t.text.secondary));
        ui.add_space(t.space.lg);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 180.0).max(0.0) / 2.0);
            if Button::new("strata.file_error.retry", "Try again").show(ui).clicked() {
                cx.dispatch(ids::LOG_RELOAD, json!({"file": file}));
            }
            if Button::new("strata.file_error.close", "Close").show(ui).clicked() {
                cx.dispatch(ids::LOG_CLOSE, json!({"file": file}));
            }
        });
    });
}

/// "The file was replaced. Reload?" (or removed) while following.
pub fn changed_banner(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, file: u64, removed: bool) {
    let (text, action) = if removed {
        ("The file was removed. Strata keeps showing what it had read.", "Close")
    } else {
        ("The file was replaced (rotated or rewritten). Reload?", "Reload")
    };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        let out = Banner::new("strata.banner.changed", Tone::Warning, text).action(action).show(ui);
        if out.action {
            let id = if removed { ids::LOG_CLOSE } else { ids::LOG_RELOAD };
            cx.dispatch(id, json!({"file": file}));
        }
    });
}

/// The "Unreadable" dialog: why a file could not be opened, and its path. Returns true when
/// dismissed.
pub fn unreadable_dialog(ctx: &egui::Context, path: &str, reason: &str) -> bool {
    let buttons = [DialogButton::new("OK").default()];
    let shown = fmt::tilde(std::path::Path::new(path));
    let out = Dialog::new("strata.unreadable", "Can't open this file", &buttons).width(440.0).show(ctx, |ui| {
        let t = ashlar::ui::tokens::current(ui.ctx());
        let r = ui.label(egui::RichText::new(reason).color(t.text.primary));
        automation::register(ui, &r, "strata.unreadable.reason", reason, Role::Label);
        ui.add_space(t.space.sm);
        let r = ui.label(egui::RichText::new(&shown).monospace().color(t.text.secondary));
        automation::register(ui, &r, "strata.unreadable.path", &shown, Role::Label);
    });
    matches!(out, Some(DialogOutcome::Button(_) | DialogOutcome::Dismissed))
}

/// Why `e` (a `LogFile::open` error's text) happened, said for people.
pub fn reason(e: &str) -> String {
    let lower = e.to_ascii_lowercase();
    if lower.contains("permission") {
        "You don't have permission to read it.".into()
    } else if lower.contains("binary") {
        "It looks like a binary file, not a text log.".into()
    } else if lower.contains("not found") || lower.contains("no such file") {
        "It doesn't exist (moved or deleted?).".into()
    } else {
        e.to_string()
    }
}
