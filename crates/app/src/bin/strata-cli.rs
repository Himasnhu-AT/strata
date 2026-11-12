//! `strata-cli commands | describe <id> | exec <id> [json] | run <file> | mcp | serve | version`,
//! and `--connect PORT` to drive the running window.

fn main() -> std::process::ExitCode {
    ashlar::app::cli_main(strata::app())
}
