//! Settings (Cmd+,): General (theme), Formats (the active file's format), Redaction (what agent
//! results hide) and AI (MCP and "Allow the agent to change the view"). A modal dialog; the
//! section is `view_keys::SETTINGS`.
//!
//! The AI section is the `agent` stream's (`agent_panel::settings_ai` after it merges); until
//! then it shows the switch and how to connect.

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::controls::{Segmented, Switch};
use ashlar::ui::dialog::{Dialog, DialogButton};
use strata_core::{FormatKind, RedactKind};

use crate::commands::ids;
use crate::doc::{OpenFile, StrataDoc, view_keys};

const SECTIONS: [&str; 4] = ["general", "formats", "redaction", "ai"];
const TITLES: [&str; 4] = ["General", "Formats", "Redaction", "AI"];

/// Draw Settings when it is open.
pub fn show(cx: &mut Cx<'_, StrataDoc>, file: Option<&OpenFile>, detected: Option<FormatKind>) {
    let Some(section) = cx.view::<String>(view_keys::SETTINGS) else { return };
    let mut at = SECTIONS.iter().position(|s| *s == section).unwrap_or(0);
    let before = at;
    let buttons = [DialogButton::new("Done").default()];
    let ctx = cx.ctx().clone();
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let allow = cx.view_or(view_keys::AGENT_ALLOW_CHANGES, true);
    let theme = cx.settings.get::<String>("theme").unwrap_or_else(|| "system".into());
    let out = Dialog::new("strata.settings", "Settings", &buttons).width(520.0).show(&ctx, |ui| {
        let t = ashlar::ui::tokens::current(ui.ctx());
        Segmented::new("strata.settings.section", "Section", &TITLES, &mut at).show(ui);
        ui.add_space(t.space.lg);
        ui.set_min_height(220.0);
        match SECTIONS.get(at).copied().unwrap_or("general") {
            "formats" => match (file, detected) {
                (Some(f), Some(kind)) => {
                    let chosen = f.format.as_deref().and_then(FormatKind::from_id);
                    ui.label(format!("Detected: {}", kind.label()));
                    ui.add_space(t.space.sm);
                    ui.label(egui::RichText::new("Read this file as:").color(t.text.secondary));
                    for k in FormatKind::ALL {
                        let on = chosen.unwrap_or(kind) == k;
                        let r = ui.radio(on, k.label());
                        automation::register(ui, &r, &format!("strata.settings.format.{}", k.as_str()), k.label(), Role::Radio);
                        if r.clicked() && !on {
                            actions.push((ids::LOG_SET_FORMAT, json!({"file": f.id.0, "format": k.as_str()})));
                        }
                    }
                }
                _ => {
                    ui.label(egui::RichText::new("Open a file to choose how it is read.").color(t.text.secondary));
                }
            },
            "redaction" => {
                if let Some(f) = file {
                    let mut on = f.redact;
                    if Switch::new("strata.settings.redact", "Redact what agents read from this file", &mut on).show(ui).changed() {
                        actions.push((ids::LOG_REDACTION, json!({"file": f.id.0, "on": on})));
                    }
                    if !f.redact {
                        ui.label(egui::RichText::new("Agents see this file's text as it is, secrets included.").color(t.status.warning));
                    }
                    ui.add_space(t.space.md);
                }
                ui.label(egui::RichText::new("Replaced with [redacted:<kind>] in agent results:").color(t.text.secondary));
                let kinds: Vec<String> = RedactKind::BUILTIN.iter().filter(|k| k.on_by_default()).map(|k| k.as_str().to_string()).collect();
                let r = ui.label(egui::RichText::new(kinds.join(", ")).monospace());
                automation::register(ui, &r, "strata.settings.redaction.kinds", &kinds.join(", "), Role::Label);
                ui.label(egui::RichText::new("The window always shows the real text; only agent results are redacted.").color(t.text.secondary));
            }
            "ai" => {
                let mut on = allow;
                if Switch::new("strata.settings.allow_changes", "Allow the agent to change the view", &mut on).show(ui).changed() {
                    actions.push((ids::AGENT_ALLOW_CHANGES, json!({"on": on})));
                }
                ui.label(
                    egui::RichText::new(
                        "Off: agents can still read (search, lines, stats), but filters, highlights, bookmarks and notes are refused.",
                    )
                    .color(t.text.secondary),
                );
                ui.add_space(t.space.md);
                ui.label("Connect Claude, Cursor or another MCP client with:");
                let r = ui.label(egui::RichText::new("strata-cli mcp").monospace().color(t.accent));
                automation::register(ui, &r, "strata.settings.mcp_command", "strata-cli mcp", Role::Label);
            }
            _ => {
                ui.label("Theme");
                let modes = ["System", "Dark", "Light"];
                let mut m = match theme.as_str() {
                    "dark" => 1,
                    "light" => 2,
                    _ => 0,
                };
                let was = m;
                Segmented::new("strata.settings.theme", "Theme", &modes, &mut m).show(ui);
                if m != was {
                    actions.push(("view.theme", json!({"mode": (["system", "dark", "light"].get(m).copied().unwrap_or("system"))})));
                }
                ui.add_space(t.space.md);
                ui.label(egui::RichText::new("Strata follows the system's light or dark mode unless you choose one.").color(t.text.secondary));
            }
        }
    });
    if at != before
        && let Some(s) = SECTIONS.get(at)
    {
        cx.set_view(view_keys::SETTINGS, s);
    }
    for (id, p) in actions {
        cx.dispatch(id, p);
    }
    if out.is_some() {
        cx.set_view(view_keys::SETTINGS, &Value::Null);
    }
}
