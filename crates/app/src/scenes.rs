//! Named scenes, one per screen and state of docs/DESIGN.md (`strata --scene NAME`,
//! `Driver::scene`, MCP `ui_scene`). The names are frozen (lead); the setups are the `ui` stream's
//! (they load fixtures from `crates/app/tests/fixtures/` once the index works).

use ashlar::prelude::*;

use crate::commands::S;
use crate::doc::view_keys;

/// Every scene name, in docs/DESIGN.md order.
pub const NAMES: &[&str] =
    &["empty", "indexing", "main", "agent", "palette", "settings", "no-matches", "file-replaced", "unreadable", "agent-disconnected"];

/// The setup of scene `name` (the stub records the name in view state for the view).
pub fn setup(name: &'static str) -> impl Fn(&mut S) + Send + Sync + 'static {
    move |s: &mut S| {
        s.set_view(view_keys::SCENE, name);
        match name {
            "agent" | "agent-disconnected" => s.set_view(view_keys::AGENT_PANEL_OPEN, &true),
            "palette" => s.request(Effect::OpenPalette),
            "settings" => s.set_view(view_keys::SETTINGS, "general"),
            _ => {}
        }
    }
}
