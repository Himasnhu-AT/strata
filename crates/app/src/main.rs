//! The Strata window. `--headless --snap out.png --tree tree.json` renders without a window;
//! `--control <port>` lets an agent drive it.

fn main() -> std::process::ExitCode {
    match strata::app().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("strata: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
