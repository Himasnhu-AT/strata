//! The agent panel (360 px, right): the agent's timeline, notes and "Undo agent changes"
//! (docs/PLAN.md §3, docs/DESIGN.md), and Settings ▸ AI. Owned by the `agent` stream.
//!
//! The `ui` stream places the panel: [`show`] draws its contents into a right side panel of
//! [`WIDTH`] points, and [`settings_ai`] draws the AI section of Settings. [`sync`] copies
//! Settings ▸ AI into view state (the switch and redaction the commands read); call it every frame
//! (the panel and the settings section call it too).
//!
//! The timeline is built with ashlar's chat kit: a virtualized [`Transcript`] whose rows are
//! [`ToolCard`]s (what the agent ran) and [`Message`]s (its notes). Runs of read-only calls fold
//! into one grey card ("Looked at 3 things"); calls that changed the view are cards of their own
//! with a "Show" button that jumps to the line.

use std::path::PathBuf;

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::chat::{ChatRole, Message, ToolCard, ToolStatus, Transcript, TranscriptItem};
use ashlar::ui::controls::{Button, Size, Switch, Variant};
use ashlar::ui::tokens;
use ashlar_agents::{Agent, Host, Op, Scope, Server, Setup, Skill, State, Window};
use strata_core::{RedactKind, Redaction};

use crate::commands::ids;
use crate::doc::{StrataDoc, view_keys};
use crate::mcp::{self, TimelineRow, keys, settings_keys};

/// The panel's width in points.
pub const WIDTH: f32 = 360.0;

/// View-state key: Settings ▸ AI was copied into view state this session.
const SYNCED: &str = "agent_settings_synced";

/// Copy Settings ▸ AI into view state once per session: "Allow the agent to change the view" and
/// the redaction of agent results.
pub fn sync(cx: &mut Cx<'_, StrataDoc>) {
    if cx.view::<bool>(SYNCED).unwrap_or(false) {
        return;
    }
    let allow = cx.settings.get::<bool>(settings_keys::ALLOW_CHANGES).unwrap_or(true);
    cx.set_view(view_keys::AGENT_ALLOW_CHANGES, &allow);
    if let Some(r) = cx.settings.get::<Redaction>(settings_keys::REDACTION) {
        cx.set_view(keys::REDACTION, &r);
    }
    cx.set_view(SYNCED, &true);
}

/// One row of the panel's transcript.
enum Item {
    /// Consecutive read-only calls.
    Reads(Vec<TimelineRow>),
    /// A call that changed the view.
    Change(usize, TimelineRow),
    /// A note (`agent.note`).
    Note(usize, TimelineRow, Option<String>),
}

struct Row {
    key: u64,
    item: Item,
}

impl TranscriptItem for Row {
    fn key(&self) -> u64 {
        self.key
    }

    fn revision(&self) -> u64 {
        match &self.item {
            Item::Reads(r) => r.len() as u64,
            Item::Change(..) => 0,
            Item::Note(_, _, text) => u64::from(text.is_some()),
        }
    }
}

/// Group the timeline: read-only runs fold into one row.
fn rows(timeline: &[TimelineRow], doc: &StrataDoc) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for (i, r) in timeline.iter().enumerate() {
        if !r.changed_view {
            if let Some(Row { item: Item::Reads(run), .. }) = out.last_mut() {
                run.push(r.clone());
                continue;
            }
            out.push(Row { key: i as u64, item: Item::Reads(vec![r.clone()]) });
        } else if r.command == ids::AGENT_NOTE {
            let text = r.item.and_then(|id| doc.agent.notes.iter().find(|n| n.id == id)).map(|n| n.text.clone());
            out.push(Row { key: i as u64, item: Item::Note(i, r.clone(), text) });
        } else {
            out.push(Row { key: i as u64, item: Item::Change(i, r.clone()) });
        }
    }
    out
}

/// Draw the panel.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    sync(cx);
    let size = egui::vec2(ui.available_width().min(WIDTH), ui.available_height());
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(size.x);
        contents(ui, cx);
    });
}

fn contents(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    let t = tokens::current(ui.ctx());
    // header: "via MCP · Claude" and the connection dot
    let client = cx.view::<String>(view_keys::AGENT_CLIENT);
    let connected = client.is_some();
    let name = client.unwrap_or_else(|| "agent".to_string());
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, if connected { t.status.success } else { t.text.secondary });
        let title = format!("via MCP · {name}");
        let label = ui.strong(&title);
        automation::register(ui, &label, "strata.agent.title", &title, Role::Label);
        let status = if connected { "connected" } else { "not connected" };
        let s = ui.weak(status);
        automation::register(ui, &s, "strata.agent.status", status, Role::Status);
    });
    ui.add_space(t.space.sm);
    // "Allow changes" and "Undo agent changes"
    ui.horizontal(|ui| {
        let mut allow = cx.view_or(view_keys::AGENT_ALLOW_CHANGES, true);
        if Switch::new("strata.agent.allow", "Allow changes", &mut allow).show(ui).changed() {
            cx.set_view(view_keys::AGENT_ALLOW_CHANGES, &allow);
            let _ = cx.settings.set(settings_keys::ALLOW_CHANGES, &allow);
            cx.dispatch(ids::AGENT_ALLOW_CHANGES, json!({"on": allow}));
        }
        let pending = cx.doc().agent.pending.len();
        let label = if pending > 0 { format!("Undo agent changes ({pending})") } else { "Undo agent changes".to_string() };
        if Button::new("strata.agent.undo", &label).variant(Variant::Secondary).size(Size::Sm).enabled(pending > 0).show(ui).clicked() {
            cx.dispatch(ids::AGENT_UNDO, json!({}));
        }
    });
    ui.add_space(t.space.sm);
    ui.separator();
    let timeline: Vec<TimelineRow> = cx.view(view_keys::AGENT_TIMELINE).unwrap_or_default();
    if timeline.is_empty() {
        let hint = "Nothing yet. What the agent reads and changes shows here.";
        let l = ui.weak(hint);
        automation::register(ui, &l, "strata.agent.empty", hint, Role::Label);
        return;
    }
    let items = rows(&timeline, cx.doc());
    let revision = (timeline.len() as u64) ^ cx.revision().0.rotate_left(32);
    let mut jump: Option<(u64, u64)> = None;
    Transcript::new("strata.agent.timeline", &items, revision).show(ui, |ui, row| match &row.item {
        Item::Reads(run) => {
            let title =
                if run.len() == 1 { run.first().map(|r| r.summary.clone()).unwrap_or_default() } else { format!("Looked at {} things", run.len()) };
            let id = format!("strata.agent.row.{}", row.key);
            ui.scope(|ui| {
                ui.visuals_mut().override_text_color = Some(t.text.secondary);
                ToolCard::new(&id, &title, ToolStatus::Succeeded)
                    .details(|ui| {
                        for r in run {
                            ui.weak(format!("{} · {}", r.command, r.summary));
                        }
                    })
                    .show(ui);
            });
        }
        Item::Change(i, r) => {
            let id = format!("strata.agent.row.{i}");
            let card = ToolCard::new(&id, &r.summary, ToolStatus::Succeeded).show(ui);
            if let (Some(file), Some(line)) = (r.file, r.line) {
                let show_id = format!("{id}.show");
                if Button::new(&show_id, "Show").variant(Variant::Ghost).size(Size::Sm).show(ui).clicked() || card.response.double_clicked() {
                    jump = Some((file, line));
                }
            }
        }
        Item::Note(i, r, text) => {
            let id = format!("strata.agent.note.{i}");
            let body = text.clone().unwrap_or_else(|| format!("~~{}~~ (undone)", r.summary));
            Message::new(&id, ChatRole::Assistant, &body).show(ui);
        }
    });
    if let Some((file, line)) = jump
        && let Some(l) = mcp::line_in(line)
    {
        cx.set_view(view_keys::ACTIVE_FILE, &file);
        cx.set_view(view_keys::SELECTED_LINE, &l);
        cx.set_view(view_keys::SCROLL_TO, &l);
    }
}

// ---------------------------------------------------------------------------------------------
// Settings ▸ AI
// ---------------------------------------------------------------------------------------------

/// What Strata installs into coding agents: the `strata` MCP server (`strata-cli mcp`) and a
/// skill with [`crate::INSTRUCTIONS`].
pub fn agent_setup(host: &Host) -> Setup {
    let exe = if cfg!(windows) { "strata-cli.exe" } else { "strata-cli" };
    let bundled: Option<PathBuf> = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(exe)));
    let command = ashlar_agents::cli_path(host, "strata-cli", bundled).map_or_else(|| "strata-cli".to_string(), |p| p.display().to_string());
    Setup {
        id: "strata".into(),
        title: "Strata".into(),
        server: Server { command, args: vec!["mcp".into()] },
        skills: vec![Skill::new("strata", "Read and search huge log files with the person, in Strata (MCP).", crate::INSTRUCTIONS)],
        window: Window::Keep,
    }
}

/// One agent's line in Settings ▸ AI.
#[derive(Clone, Debug)]
struct AgentLine {
    agent: Agent,
    found: bool,
    state: Option<State>,
    error: Option<String>,
}

fn overview() -> Vec<AgentLine> {
    let Some(host) = Host::current() else { return Vec::new() };
    let setup = agent_setup(&host);
    ashlar_agents::overview(&setup, &Scope::User, &host)
        .into_iter()
        .map(|a| AgentLine { agent: a.agent, found: a.found, state: a.state, error: a.error })
        .collect()
}

/// Install or remove Strata in `agent`; the text for a toast.
fn apply(agent: Agent, install: bool) -> Result<String, String> {
    let host = Host::current().ok_or("no home folder")?;
    let setup = agent_setup(&host);
    let op = if install { Op::Install(&setup) } else { Op::Uninstall(&setup.id) };
    let outcome = ashlar_agents::run(op, &[agent], &Scope::User, &host, true, "strata").into_iter().next().ok_or("nothing to do")?;
    match outcome.error {
        Some(e) => Err(format!("{}: {e}", agent.title())),
        None if install => Ok(format!("Connected {}. Restart it to pick this up.", agent.title())),
        None => Ok(format!("Disconnected {}.", agent.title())),
    }
}

/// Settings ▸ AI: connect coding agents, "Allow the agent to change the view" and redaction of
/// agent results.
pub fn settings_ai(ui: &mut egui::Ui, cx: &mut Cx<'_, StrataDoc>) {
    sync(cx);
    let t = tokens::current(ui.ctx());
    let h = ui.heading("AI");
    automation::register(ui, &h, "strata.settings.ai.title", "AI", Role::Label);
    ui.weak("Agents read the log through Strata's MCP server. Nothing leaves the app unless you connect one.");
    ui.add_space(t.space.md);

    // --- agents ---------------------------------------------------------------------------------
    ui.strong("Coding agents");
    let cache = egui::Id::new("strata.settings.ai.agents");
    let mut lines: Option<Vec<AgentLine>> = ui.data(|d| d.get_temp(cache));
    if lines.is_none() || Button::new("strata.settings.ai.refresh", "Check again").variant(Variant::Ghost).size(Size::Sm).show(ui).clicked() {
        lines = Some(overview());
    }
    let mut changed = false;
    for a in lines.clone().unwrap_or_default() {
        ui.horizontal(|ui| {
            let state = match (&a.error, a.state, a.found) {
                (Some(e), _, _) => format!("can't read its settings: {e}"),
                (None, Some(State::Current), _) => "connected".to_string(),
                (None, Some(State::Outdated), _) => "connected (needs an update)".to_string(),
                (None, _, true) => "not connected".to_string(),
                (None, _, false) => "not found on this Mac".to_string(),
            };
            let text = format!("{} · {state}", a.agent.title());
            let l = ui.label(&text);
            automation::register(ui, &l, &format!("strata.settings.ai.agent.{}", a.agent.id()), &text, Role::Label);
            let installed = matches!(a.state, Some(State::Current | State::Outdated));
            let (id, label, install) = match a.state {
                Some(State::Current) => (format!("strata.settings.ai.agent.{}.remove", a.agent.id()), "Disconnect", false),
                Some(State::Outdated) => (format!("strata.settings.ai.agent.{}.install", a.agent.id()), "Update", true),
                _ => (format!("strata.settings.ai.agent.{}.install", a.agent.id()), "Connect", true),
            };
            let enabled = a.error.is_none() && (a.found || installed);
            if Button::new(&id, label).size(Size::Sm).enabled(enabled).show(ui).clicked() {
                let (level, msg) = match apply(a.agent, install) {
                    Ok(m) => (ToastLevel::Success, m),
                    Err(e) => (ToastLevel::Error, e),
                };
                cx.session_mut().toast(level, msg);
                changed = true;
            }
        });
    }
    if changed {
        lines = Some(overview());
    }
    if let Some(l) = lines {
        ui.data_mut(|d| d.insert_temp(cache, l));
    }
    ui.add_space(t.space.md);

    // --- agent changes ----------------------------------------------------------------------------
    ui.strong("Agent changes");
    let mut allow = cx.view_or(view_keys::AGENT_ALLOW_CHANGES, true);
    if Switch::new("strata.settings.ai.allow", "Allow the agent to change the view (filters, highlights, bookmarks)", &mut allow).show(ui).changed() {
        cx.set_view(view_keys::AGENT_ALLOW_CHANGES, &allow);
        let _ = cx.settings.set(settings_keys::ALLOW_CHANGES, &allow);
        cx.dispatch(ids::AGENT_ALLOW_CHANGES, json!({"on": allow}));
    }
    ui.weak("Off: the agent can only read; view.*, bookmark.add and agent.note return an error.");
    ui.add_space(t.space.md);

    // --- redaction ----------------------------------------------------------------------------------
    ui.strong("Redaction of what the agent reads");
    ui.weak("Secrets in log lines are replaced with [redacted:<kind>] before an agent sees them.");
    let mut red: Redaction = cx.view(keys::REDACTION).unwrap_or_default();
    let mut dirty = false;
    for kind in RedactKind::BUILTIN {
        let mut on = red.is_on(&kind);
        let id = format!("strata.settings.ai.redact.{}", kind.as_str());
        if Switch::new(&id, kind.as_str(), &mut on).show(ui).changed() {
            red.set(kind, on);
            dirty = true;
        }
    }
    if dirty {
        red.enabled = true;
        cx.set_view(keys::REDACTION, &red);
        let _ = cx.settings.set(settings_keys::REDACTION, &red);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fold_and_changes_stand_alone() {
        let r = |command: &str, changed_view: bool| TimelineRow { command: command.into(), changed_view, ..TimelineRow::default() };
        let tl = vec![
            r(ids::LOG_SEARCH, false),
            r(ids::LOG_AROUND, false),
            r(ids::VIEW_HIGHLIGHT, true),
            r(ids::LOG_LINES, false),
            r(ids::AGENT_NOTE, true),
        ];
        let rows = rows(&tl, &StrataDoc::default());
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[0].item, Item::Reads(run) if run.len() == 2));
        assert!(matches!(&rows[1].item, Item::Change(2, _)));
        assert!(matches!(&rows[3].item, Item::Note(4, _, None)));
    }

    #[test]
    fn the_setup_installs_the_mcp_server() {
        let dir = std::env::temp_dir();
        let host = Host::at(dir, ashlar_agents::Os::current());
        let s = agent_setup(&host);
        assert_eq!(s.id, "strata");
        assert_eq!(s.server.args, vec!["mcp".to_string()]);
        assert!(s.server.command.contains("strata-cli"));
    }
}
