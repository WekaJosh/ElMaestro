//! Engine: SSH + service + coordinator + pre-flight checks.

pub mod check;
pub mod coordinator;
pub mod service;
pub mod ssh;

pub use coordinator::{run as run_spec, CoordinatorError};
