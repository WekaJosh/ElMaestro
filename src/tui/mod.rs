//! ratatui TUI.
//!
//! Public API:
//!   - `run_home()`   open at the home menu (default when `elmaestro` has no args)
//!   - `run_tui(cfg)` open straight to a config's run screen

pub mod app;
pub mod configure;
pub mod runner;
pub mod screens;

pub use app::{run_home, run_tui};
