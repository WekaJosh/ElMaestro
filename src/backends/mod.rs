//! Backends: trait + elbencho + fio implementations.
//!
//! Stub for now. Will be filled in once config + results modules compile.

use crate::config::Target;
use crate::results::schema::{EngineArtifactRefs, PhaseResult};

#[derive(Debug, Clone)]
pub struct EngineVersion {
    pub raw: String,
    pub version: Option<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TargetSupport {
    pub supported: bool,
    pub reason: String,
}

/// What every engine implements. Async because the SSH layer is async.
#[allow(dead_code)]
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn supports_target(&self, target: &Target) -> TargetSupport;
}

// Stubs for the actual backends, to be replaced when SSH/coordinator are ported.
pub struct ElbenchoBackend;
pub struct FioBackend;

impl Backend for ElbenchoBackend {
    fn name(&self) -> &'static str {
        "elbencho"
    }
    fn supports_target(&self, _target: &Target) -> TargetSupport {
        TargetSupport {
            supported: true,
            reason: String::new(),
        }
    }
}

impl Backend for FioBackend {
    fn name(&self) -> &'static str {
        "fio"
    }
    fn supports_target(&self, target: &Target) -> TargetSupport {
        match target {
            Target::Posix(_) => TargetSupport {
                supported: true,
                reason: String::new(),
            },
            Target::S3(_) => TargetSupport {
                supported: false,
                reason: "fio S3 support is roadmap; use engine: elbencho for S3 targets".into(),
            },
        }
    }
}

/// Suppress dead-code warning for unused imports while these stubs live.
#[allow(dead_code)]
fn _anchor(_: &PhaseResult, _: &EngineArtifactRefs) {}
