//! The window: toolbar, sidebar, histogram strip, log table, detail pane, status bar, empty and
//! error states (docs/DESIGN.md). Owned by the `ui` stream: one file per part under `views/`.
//!
//! Views read the document and view state and dispatch commands; they never change state
//! directly. Every control registers an automation id (`strata.<part>.<name>`).

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};

use crate::commands::ids;
use crate::doc::{StrataDoc, view_keys};

/// The view function given to `App::view`.
pub fn view() -> impl FnMut(&mut egui::Ui, &mut Cx<'_, StrataDoc>) {
    move |ui, cx| {
        if cx.doc().files.is_empty() {
            empty_state(ui, cx);
        } else {
            ui.label("Log table: not yet");
        }
        if cx.view::<bool>(view_keys::AGENT_PANEL_OPEN).unwrap_or(false) {
            crate::agent_panel::show(ui, cx);
        }
    }
}

/// Screen 1, "Open / empty": a drop target, "Open a file", recent files and a line on the agent.
fn empty_state(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        let title = ui.heading("Drop a log file here");
        automation::register(ui, &title, "strata.empty.title", "Drop a log file here", Role::Label);
        let open = ui.button("Open a file");
        automation::register(ui, &open, "strata.empty.open", "Open a file", Role::Button);
        if open.clicked() {
            cx.dispatch(ids::LOG_OPEN_DIALOG, json!({}));
        }
        let hint = ui.weak("Connect an AI agent in Settings > AI");
        automation::register(ui, &hint, "strata.empty.agent", "Connect an AI agent in Settings > AI", Role::Label);
    });
}
