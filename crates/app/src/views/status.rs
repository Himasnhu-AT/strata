//! The status bar (24 pt): "1,284,113 lines · 1.9 GB · JSON lines · 2 filters · following ·
//! 74 MB · built with ashlar", or the indexing progress ("Indexing 38 % · 1.9 of 5.0 GB · Esc to
//! stop").

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::status_bar::{self, StatusBar};

use super::fmt;
use crate::doc::StrataDoc;

/// What the bar says.
#[derive(Default)]
pub struct StatusIn {
    pub lines: Option<u64>,
    pub bytes: Option<u64>,
    pub format: Option<&'static str>,
    pub filters: usize,
    pub shown: Option<u64>,
    pub following: bool,
    /// Indexing: done fraction, bytes done, bytes in all.
    pub indexing: Option<(f32, u64, u64)>,
    pub memory: Option<u64>,
}

fn item(ui: &mut egui::Ui, id: &str, text: &str, color: egui::Color32) {
    let r = ui.label(egui::RichText::new(text).size(11.0).color(color));
    automation::register(ui, &r, id, text, Role::Status);
}

/// Draw the bar along the bottom of `ui`.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>, s: &StatusIn) {
    let t = cx.tokens();
    StatusBar::new("strata.status").show(
        ui,
        |ui| {
            if let Some((frac, done, total)) = s.indexing {
                let pct = (frac * 100.0).floor().clamp(0.0, 100.0);
                item(
                    ui,
                    "strata.status.indexing",
                    &format!("Indexing {pct:.0} % · {} of {} · Esc to stop", fmt::bytes(done), fmt::bytes(total)),
                    t.accent,
                );
                if let Some(l) = s.lines {
                    item(ui, "strata.status.lines", &format!("{} lines so far", fmt::thousands(l)), t.text.secondary);
                }
                return;
            }
            if let Some(l) = s.lines {
                item(ui, "strata.status.lines", &format!("{} lines", fmt::thousands(l)), t.text.secondary);
            }
            if let Some(b) = s.bytes {
                item(ui, "strata.status.size", &fmt::bytes(b), t.text.secondary);
            }
            if let Some(f) = s.format {
                item(ui, "strata.status.format", f, t.text.secondary);
            }
            if s.filters > 0 {
                let n = if s.filters == 1 { "1 filter".to_string() } else { format!("{} filters", s.filters) };
                let text = match s.shown {
                    Some(shown) => format!("{n} · {} shown", fmt::thousands(shown)),
                    None => n,
                };
                item(ui, "strata.status.filters", &text, t.text.secondary);
            }
            if s.following {
                item(ui, "strata.status.follow", "● following", t.status.success);
            }
            if let Some(m) = s.memory {
                item(ui, "strata.status.memory", &fmt::bytes(m), t.text.secondary);
            }
        },
        |ui| {
            status_bar::text(ui, "strata.status.ashlar", "built with ashlar");
        },
    );
}
