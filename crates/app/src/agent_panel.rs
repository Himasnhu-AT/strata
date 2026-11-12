//! The agent panel (360 px, right): the agent's timeline, notes and "Undo agent changes"
//! (docs/PLAN.md §3, docs/DESIGN.md). Owned by the `agent` stream.

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};

use crate::doc::StrataDoc;

/// Draw the panel.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    let _ = &cx.doc().agent;
    let title = ui.label("Agent: not yet");
    automation::register(ui, &title, "strata.agent.title", "Agent", Role::Label);
}
