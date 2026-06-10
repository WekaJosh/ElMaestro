//! Backends: `Backend` trait + elbencho + fio implementations + registry.
//!
//! Each backend translates a RunSpec into a concrete subprocess invocation
//! and parses the output back into the canonical PhaseResult schema. The
//! coordinator stays engine-agnostic above this line.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{ClientHost, Engine, RunSpec, Target};
use crate::results::schema::{EngineArtifactRefs, PhaseResult};

pub mod elbencho;
pub mod fio;

pub use elbencho::ElbenchoBackend;
pub use fio::FioBackend;

/// Point-in-time stats parsed from an engine's live output while a run
/// is still in flight. Values are cumulative-so-far averages (fio) or
/// the latest interval (elbencho); either way they're for the Run
/// screen's live display, not for the final report.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveStats {
    pub throughput_mib_s: Option<f64>,
    pub iops: Option<f64>,
}

/// Parsed output of `<binary> --version`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineVersion {
    pub raw: String,
    pub version: Option<String>,
    pub features: Vec<String>,
}

impl EngineVersion {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f.eq_ignore_ascii_case(feature))
    }
}

/// Whether a backend supports a given target. When `supported` is false,
/// `reason` explains.
#[derive(Debug, Clone)]
pub struct TargetSupport {
    pub supported: bool,
    pub reason: String,
}

impl TargetSupport {
    pub fn yes() -> Self {
        TargetSupport {
            supported: true,
            reason: String::new(),
        }
    }
    pub fn no(reason: impl Into<String>) -> Self {
        TargetSupport {
            supported: false,
            reason: reason.into(),
        }
    }
}

/// The contract every benchmark engine implements.
pub trait Backend: Send + Sync {
    /// Engine name as used in YAML's top-level `engine:` field.
    fn name(&self) -> &'static str;

    /// Parse `<binary> --version`. Runs the local binary by subprocess.
    fn detect_version(&self, local_path: &str) -> Result<EngineVersion>;

    /// Construct the engine command for one RunSpec.
    ///
    /// `raw_dir` is the directory where the engine writes its output files;
    /// the backend chooses its own filenames within. `hosts` is the
    /// comma-separated host:port list for multi-client fan-out (None for
    /// single-host).
    ///
    /// Returns (argv, primary_phase) where primary_phase is the phase whose
    /// numbers headline the report (read | write | mixed).
    fn build_argv(
        &self,
        spec: &RunSpec,
        raw_dir: &Path,
        local_path: &str,
        hosts: Option<&str>,
    ) -> Result<(Vec<String>, String)>;

    /// Parse the engine's output files in `raw_dir`. Returns the phases dict
    /// + the artifact-refs struct (with engine-specific path fields filled).
    fn parse_results(
        &self,
        raw_dir: &Path,
        command: &str,
    ) -> Result<(std::collections::HashMap<String, PhaseResult>, EngineArtifactRefs)>;

    /// Whether this backend can drive the given target.
    fn supports_target(&self, target: &Target) -> TargetSupport;

    /// argv to start the engine's service/server mode on a remote host.
    fn service_command(&self, client: &ClientHost) -> Vec<String>;
}

/// Look up a backend instance by engine name. Returns a boxed trait object
/// so callers don't have to thread a generic through.
pub fn get_backend(engine: Engine) -> Box<dyn Backend> {
    match engine {
        Engine::Elbencho => Box::new(ElbenchoBackend::new()),
        Engine::Fio => Box::new(FioBackend::new()),
    }
}
