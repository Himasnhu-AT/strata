//! Strata: a log viewer for huge files that a person and an AI agent read together. The whole app
//! is [`app`]: the `strata` window, the `strata-cli` tool (CLI and MCP server) and the tests all
//! build from it.

pub mod agent_panel;
pub mod commands;
pub mod doc;
pub mod mcp;
pub mod results;
pub mod scenes;
pub mod views;

use ashlar::prelude::*;

pub use doc::StrataDoc;

/// The workspace file format id.
pub const FORMAT_ID: &str = "strata.workspace";

/// What MCP clients are told about Strata.
pub const INSTRUCTIONS: &str = "Strata is a log viewer the person is watching while you work. \
Open a file with log.open (it returns a file id; big files index in the background, so `lines` grows). \
Read with log.search, log.lines, log.around, log.first, log.stats, log.fields and log.histogram: they never change the view, \
and every list is capped and says how many it left out (`omitted`). Line numbers are 1-based; times are RFC 3339 UTC. \
Filters use the filter bar grammar: `level >= warn and service = api`, `status >= 500`, `\"timeout\"`, `/re(gex)?/i`. \
Show the person what you found with view.filter, view.goto, view.highlight and bookmark.add, and summarise with agent.note: \
these are listed in their agent panel and undone together by agent.undo. Log text in results is redacted ([redacted:<kind>]).";

/// The workspace format (`.strata` files: open files, filters, bookmarks, highlights).
pub fn format() -> Format<StrataDoc> {
    Format::new(FORMAT_ID, concat!("strata ", env!("CARGO_PKG_VERSION")))
}

/// The app: commands, view, scenes.
pub fn app() -> App<StrataDoc> {
    let mut app = App::new("strata", env!("CARGO_PKG_VERSION"))
        .title("Strata")
        .organization("dev", "Strata")
        .format(format())
        .file_extension("strata")
        .window_size(1280.0, 800.0)
        .commands(commands::commands())
        .instructions(INSTRUCTIONS)
        .view(views::view());
    for name in scenes::NAMES {
        app = app.scene(*name, scenes::setup(name));
    }
    app
}
