//! Engine: SSH + service + coordinator.

pub mod coordinator;
pub mod service;
pub mod ssh;

pub use coordinator::{run as run_spec, CoordinatorError};
