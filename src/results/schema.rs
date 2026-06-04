//! Canonical Result schema. Stubs for now; filled in when results parsing
//! comes online. Field names mirror the v1.0 JSON schema produced by the
//! Python version so existing result.json files load.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyBucket {
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub avg: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseResult {
    pub operation: String,
    #[serde(default)]
    pub throughput_mib_s_first: Option<f64>,
    #[serde(default)]
    pub throughput_mib_s_last: Option<f64>,
    #[serde(default)]
    pub iops_first: Option<f64>,
    #[serde(default)]
    pub iops_last: Option<f64>,
    #[serde(default)]
    pub ops_per_s_first: Option<f64>,
    #[serde(default)]
    pub ops_per_s_last: Option<f64>,
    #[serde(default)]
    pub entries: Option<f64>,
    #[serde(default)]
    pub mib_total: Option<f64>,
    #[serde(default)]
    pub cpu_pct: Option<f64>,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub io_lat_us: LatencyBucket,
    #[serde(default)]
    pub ent_lat_us: LatencyBucket,
    #[serde(default)]
    pub latency_percentiles_us: HashMap<String, f64>,
    #[serde(default)]
    pub raw: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientInfo {
    pub host: String,
    #[serde(default)]
    pub elbencho_version: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    /// Hardware facts gathered from this client at run time (CPU, RAM,
    /// NICs, OS). None when gathering failed or wasn't attempted.
    #[serde(default)]
    pub system: Option<SystemInfo>,
}

/// A single network interface's reported link speed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NicInfo {
    pub name: String,
    /// Link speed in Mbit/s as reported by /sys/class/net/<if>/speed.
    pub speed_mbps: u64,
}

/// Hardware / OS facts for one client, gathered at run time. Every field
/// is optional because the gather command degrades gracefully when a
/// tool isn't installed or a fact needs root (e.g. DIMM speed via
/// dmidecode).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub cpu_count: Option<u32>,
    #[serde(default)]
    pub mem_total_bytes: Option<u64>,
    /// e.g. "DDR4", "DDR5" (dmidecode, root-only — often None).
    #[serde(default)]
    pub mem_type: Option<String>,
    /// e.g. "3200 MT/s" (dmidecode, root-only — often None).
    #[serde(default)]
    pub mem_speed: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub nics: Vec<NicInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineArtifactRefs {
    pub command: String,
    pub stdout_path: String,
    #[serde(default)]
    pub csv_path: Option<String>,
    #[serde(default)]
    pub jsonfile_path: Option<String>,
    #[serde(default)]
    pub livecsv_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetSnapshot {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub detail: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadSnapshot {
    pub name: String,
    pub block_size: u64,
    pub rw_mix_pct_read: u8,
    pub threads_per_client: u32,
    pub io_depth: u32,
    pub pattern: String,
    pub direct_io: bool,
    #[serde(default)]
    pub duration_s: Option<u64>,
    #[serde(default)]
    pub dataset_size: Option<u64>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_count: Option<u32>,
    pub total_concurrency: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Result {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub run_id: String,
    pub spec_hash: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    pub primary_phase: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_s: f64,
    pub target: TargetSnapshot,
    pub workload: WorkloadSnapshot,
    pub clients: Vec<ClientInfo>,
    /// Field name is historical (predates fio support).
    pub elbencho: EngineArtifactRefs,
    pub phases: HashMap<String, PhaseResult>,
    pub elbencho_exit_code: i32,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

fn default_schema_version() -> String {
    SCHEMA_VERSION.into()
}

fn default_engine() -> String {
    "elbencho".into()
}
