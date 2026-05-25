//! Config: YAML schemas, loader with placeholder expansion, sweep expansion.
//!
//! Mirrors python-legacy/src/elbencho_harness/config/{models.py, loader.py,
//! sweep.py}. The Python tests in python-legacy/tests/unit/test_models.py /
//! test_loader_expand.py / test_sweep.py serve as oracles.

pub mod loader;
pub mod models;
pub mod sweep;

pub use models::{
    ByteSize, ClientHost, Engine, PosixTarget, RunPlan, RunRef, RunSpec, S3Target, Sweep,
    SweepAxis, Target, Workload,
};
