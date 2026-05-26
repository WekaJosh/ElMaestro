//! HTML report rendering. Charts are emitted as Plotly.js JSON specs
//! and embedded in askama-rendered HTML templates.

pub mod charts;
pub mod compare;
pub mod render;

pub use compare::{load_run, render_compare, LoadedRun};
pub use render::render_single;
