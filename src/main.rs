//! ElMaestro: IO benchmarking harness.
//!
//! Binary entry. Delegates to the CLI module, which itself dispatches to
//! either the TUI or one of the scripted subcommands.

use std::process::ExitCode;

fn main() -> ExitCode {
    match elmaestro::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
