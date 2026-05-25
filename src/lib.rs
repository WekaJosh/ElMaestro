//! ElMaestro library crate.
//!
//! Top-level modules:
//!   - `config`:    YAML schemas + loader + sweep expansion
//!   - `results`:   canonical Result schema + on-disk store
//!   - `backends`:  Backend trait + elbencho + fio impls
//!   - `engine`:    coordinator (dispatch local vs SSH fan-out) + ssh + service
//!   - `report`:    HTML report rendering (single + compare)
//!   - `tui`:       Textual-equivalent UI on ratatui
//!   - `cli`:       clap subcommand wiring

pub mod cli;
pub mod config;
pub mod results;

// Modules below get filled in as the rewrite progresses.
pub mod backends;
pub mod engine;
pub mod report;
pub mod tui;

/// Crate version, exposed so `elmaestro version` shows the right thing.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
