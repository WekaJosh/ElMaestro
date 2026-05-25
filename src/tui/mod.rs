//! Textual-equivalent TUI on ratatui. Stub until ported.
//!
//! Provides minimal `run_home` / `run_tui` shims so cli.rs links until the
//! real ratatui app lands.

use std::path::Path;

use anyhow::Result;

/// Open the TUI at the home menu. Placeholder until the ratatui port lands.
pub fn run_home() -> Result<()> {
    println!("ElMaestro {} — TUI not yet ported to Rust.", crate::VERSION);
    println!("Use a subcommand for now: elmaestro --help");
    Ok(())
}

/// Open the TUI for a specific config. Placeholder.
pub fn run_tui(config: Option<&Path>) -> Result<()> {
    match config {
        Some(path) => println!("[TUI placeholder] would open with config: {}", path.display()),
        None => println!("[TUI placeholder] would open home screen"),
    }
    Ok(())
}
