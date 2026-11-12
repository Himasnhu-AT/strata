//! `cargo xtask <task>`: the repo's gates and tools.
//!
//! - `ci`: fmt check, clippy `-D warnings` on every target, then the tests (`--no-fail-fast`).
//! - `fixtures [--size 100M] [--format plain|jsonl|logfmt|syslog|access] [--seed N] [--out PATH]`:
//!   write a realistic log file, the same bytes for the same arguments (see `fixtures.rs`).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod fixtures;

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// The workspace root (the parent of `xtask/`).
fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

fn run(what: &str, args: &[&str]) -> Result<(), String> {
    eprintln!("xtask: cargo {}", args.join(" "));
    let status = Command::new(cargo()).current_dir(root()).args(args).status().map_err(|e| format!("{what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what} failed ({status})")) }
}

fn ci() -> Result<(), String> {
    run("fmt", &["fmt", "--all", "--", "--check"])?;
    run("clippy", &["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"])?;
    run("tests", &["test", "--workspace", "--locked", "--no-fail-fast"])
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(),
        Some("fixtures") => fixtures::main(args.get(1..).unwrap_or_default()),
        _ => Err("usage: cargo xtask ci | fixtures [--size 100M] [--format plain|jsonl|logfmt|syslog|access] [--seed N] [--out PATH]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
