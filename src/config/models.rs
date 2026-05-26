//! Canonical config models.
//!
//! Single source of truth for YAML and the TUI editor. Uses serde for
//! parsing + validation. ByteSize accepts both integers and human-readable
//! strings ("1MiB", "4k") thanks to a custom Deserialize impl.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

/// Bytes value. Deserialized from either an integer or a human-readable
/// string like "1MiB", "4k", "256MB". Suffixes are 1024-based (binary)
/// regardless of how they're spelled, matching `humanfriendly.parse_size(
/// v, binary=True)` in the Python implementation.
pub type ByteSize = u64;

/// Which IO engine drives the workload. Defaults to elbencho for backward
/// compatibility with v0.1-v0.6 configs that didn't have the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Elbencho,
    Fio,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::Elbencho
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Engine::Elbencho => f.write_str("elbencho"),
            Engine::Fio => f.write_str("fio"),
        }
    }
}

// ---------------------------------------------------------------------------
// ClientHost
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHost {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub ssh_user: Option<String>,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default)]
    pub ssh_key: Option<PathBuf>,
    /// Optional jump host (ssh -J). Lets a user on their laptop reach
    /// internal workers via a bastion. Accepts `host`, `user@host`,
    /// or `user@host:port`.
    #[serde(default)]
    pub ssh_jump: Option<String>,
    /// Path to the engine binary on this client (historical name; works
    /// for both elbencho and fio).
    #[serde(default = "default_engine_path")]
    pub elbencho_path: String,
    #[serde(default = "default_service_port")]
    pub service_port: u16,
}

impl Default for ClientHost {
    fn default() -> Self {
        Self {
            host: default_host(),
            ssh_user: None,
            ssh_port: default_ssh_port(),
            ssh_key: None,
            ssh_jump: None,
            elbencho_path: default_engine_path(),
            service_port: default_service_port(),
        }
    }
}

fn default_host() -> String {
    "localhost".into()
}

fn default_ssh_port() -> u16 {
    22
}

fn default_engine_path() -> String {
    "elbencho".into()
}

fn default_service_port() -> u16 {
    1611
}

// ---------------------------------------------------------------------------
// Targets (discriminated union on `kind`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PosixTarget {
    pub name: String,
    pub mount_path: PathBuf,
    #[serde(default = "default_dataset_subdir")]
    pub dataset_subdir: String,
    #[serde(default = "default_cleanup")]
    pub cleanup: bool,
}

fn default_dataset_subdir() -> String {
    "elbencho-bench".into()
}

fn default_cleanup() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Target {
    pub name: String,
    pub endpoint: String,
    pub bucket: String,
    #[serde(default)]
    pub region: Option<String>,
    /// Reference to credentials. Must be `env:NAME` or `file:/path`. Inline
    /// secrets are rejected.
    pub credentials_ref: String,
    #[serde(default = "default_addressing")]
    pub addressing: String,
}

fn default_addressing() -> String {
    "path".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    Posix(PosixTarget),
    S3(S3Target),
}

impl Target {
    pub fn name(&self) -> &str {
        match self {
            Target::Posix(t) => &t.name,
            Target::S3(t) => &t.name,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Target::Posix(t) => {
                if t.dataset_subdir.starts_with('/')
                    || std::path::Path::new(&t.dataset_subdir)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    anyhow::bail!("dataset_subdir must be a relative path with no '..'");
                }
            }
            Target::S3(t) => {
                if !(t.credentials_ref.starts_with("env:") || t.credentials_ref.starts_with("file:"))
                {
                    anyhow::bail!(
                        "credentials_ref must be 'env:NAME' or 'file:/path'; \
                         inline secrets are rejected"
                    );
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Workload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub name: String,
    #[serde(default = "default_pattern")]
    pub pattern: String,
    #[serde(default = "default_rw_mix")]
    pub rw_mix_pct_read: u8,
    #[serde(deserialize_with = "de_bytesize")]
    pub block_size: ByteSize,
    #[serde(default = "default_threads")]
    pub threads_per_client: u32,
    #[serde(default = "default_io_depth")]
    pub io_depth: u32,
    #[serde(default = "default_direct_io")]
    pub direct_io: bool,
    #[serde(default)]
    pub sync_after_write: bool,
    #[serde(default)]
    pub drop_caches_before: bool,
    #[serde(default)]
    pub duration_s: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_bytesize")]
    pub dataset_size: Option<ByteSize>,
    #[serde(default, deserialize_with = "de_opt_bytesize")]
    pub file_size: Option<ByteSize>,
    #[serde(default)]
    pub file_count: Option<u32>,
    /// S3-only: multipart upload chunk size. Ignored for POSIX.
    #[serde(default, deserialize_with = "de_opt_bytesize")]
    pub s3_multipart_size: Option<ByteSize>,
    /// S3-only: object key prefix. Ignored for POSIX.
    #[serde(default)]
    pub s3_object_prefix: Option<String>,
    /// Free-form extra flags. Appended verbatim to the engine's command line
    /// (elbencho) or to the job file (fio).
    #[serde(default)]
    pub extra_flags: Vec<String>,
}

fn default_pattern() -> String {
    "seq".into()
}

fn default_rw_mix() -> u8 {
    100
}

fn default_threads() -> u32 {
    1
}

fn default_io_depth() -> u32 {
    1
}

fn default_direct_io() -> bool {
    true
}

impl Workload {
    pub fn validate(&self) -> Result<()> {
        if self.rw_mix_pct_read > 100 {
            anyhow::bail!("rw_mix_pct_read must be 0..=100");
        }
        if !(self.pattern == "seq" || self.pattern == "rand") {
            anyhow::bail!("pattern must be 'seq' or 'rand'");
        }
        let has_duration = self.duration_s.is_some();
        let has_dataset = self.dataset_size.is_some() || self.file_size.is_some();
        if !has_duration && !has_dataset {
            anyhow::bail!(
                "workload must specify either duration_s OR (dataset_size and/or file_size)"
            );
        }
        Ok(())
    }

    /// total_concurrency = threads_per_client * client_count.
    pub fn total_concurrency(&self, client_count: usize) -> u64 {
        self.threads_per_client as u64 * client_count.max(1) as u64
    }
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepAxis {
    #[serde(default, deserialize_with = "de_opt_bytesize_list")]
    pub block_size: Option<Vec<ByteSize>>,
    #[serde(default)]
    pub rw_mix_pct_read: Option<Vec<u8>>,
    #[serde(default)]
    pub threads_per_client: Option<Vec<u32>>,
    #[serde(default)]
    pub io_depth: Option<Vec<u32>>,
    #[serde(default)]
    pub client_count: Option<Vec<usize>>,
    #[serde(default, deserialize_with = "de_opt_bytesize_list")]
    pub dataset_size: Option<Vec<ByteSize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub name: String,
    /// Workload name reference (base values to override per sweep point).
    pub base: String,
    #[serde(default)]
    pub targets: Option<Vec<String>>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub axes: SweepAxis,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default)]
    pub max_runs: Option<usize>,
}

fn default_order() -> String {
    "cartesian".into()
}

// ---------------------------------------------------------------------------
// Run references + RunSpec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRef {
    pub target: String,
    pub workload: String,
}

/// One concrete materialized run (target + workload + clients), built by
/// the loader or sweep expander. Not declared in YAML.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub run_id: String,
    pub spec_hash: String,
    pub target: Target,
    pub workload: Workload,
    pub clients: Vec<ClientHost>,
}

impl RunSpec {
    pub fn target_name(&self) -> &str {
        self.target.name()
    }

    /// sha256 of normalized spec for dedup / resume.
    pub fn make_spec_hash(target: &Target, workload: &Workload, clients: &[ClientHost]) -> String {
        // Match Python's behavior: serialize to canonical JSON, hash.
        // Field ordering must be stable: serde_json uses BTreeMap when sort_keys
        // is on, but we already use struct fields which serialize in source order.
        // For determinism we serialize to a sorted JSON Value first.
        let payload = serde_json::json!({
            "target": serde_json::to_value(target).unwrap_or_default(),
            "workload": serde_json::to_value(workload).unwrap_or_default(),
            "clients": clients
                .iter()
                .map(|c| serde_json::to_value(c).unwrap_or_default())
                .collect::<Vec<_>>(),
        });
        let normalized = canonicalize_json(&payload);
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

/// Produce a canonical, sort-key JSON string of a serde_json::Value so the
/// hash is stable regardless of field declaration order.
fn canonicalize_json(v: &serde_json::Value) -> String {
    fn write(v: &serde_json::Value, buf: &mut String) {
        match v {
            serde_json::Value::Null => buf.push_str("null"),
            serde_json::Value::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
            serde_json::Value::Number(n) => buf.push_str(&n.to_string()),
            serde_json::Value::String(s) => {
                buf.push_str(&serde_json::to_string(s).expect("string serializes"));
            }
            serde_json::Value::Array(items) => {
                buf.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        buf.push(',');
                    }
                    write(item, buf);
                }
                buf.push(']');
            }
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                buf.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        buf.push(',');
                    }
                    buf.push_str(&serde_json::to_string(*k).expect("key serializes"));
                    buf.push(':');
                    write(&map[*k], buf);
                }
                buf.push('}');
            }
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

// ---------------------------------------------------------------------------
// RunPlan (top-level)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPlan {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub engine: Engine,
    #[serde(default = "default_output_dir")]
    pub output_dir: PathBuf,
    #[serde(default = "default_clients")]
    pub clients: Vec<ClientHost>,
    pub targets: Vec<Target>,
    pub workloads: Vec<Workload>,
    #[serde(default)]
    pub runs: Vec<RunRef>,
    #[serde(default)]
    pub sweeps: Vec<Sweep>,
}

fn default_version() -> u32 {
    1
}

fn default_output_dir() -> PathBuf {
    PathBuf::from("./results")
}

fn default_clients() -> Vec<ClientHost> {
    vec![ClientHost::default()]
}

impl RunPlan {
    /// Cross-field validation: target/workload names unique; runs/sweeps
    /// reference existing names; per-target and per-workload internal
    /// validation.
    pub fn validate(&self) -> Result<()> {
        let target_names: std::collections::HashSet<&str> =
            self.targets.iter().map(|t| t.name()).collect();
        if target_names.len() != self.targets.len() {
            anyhow::bail!("duplicate target names");
        }
        let workload_names: std::collections::HashSet<&str> =
            self.workloads.iter().map(|w| w.name.as_str()).collect();
        if workload_names.len() != self.workloads.len() {
            anyhow::bail!("duplicate workload names");
        }
        for t in &self.targets {
            t.validate()?;
        }
        for w in &self.workloads {
            w.validate()?;
        }
        for (i, r) in self.runs.iter().enumerate() {
            if !target_names.contains(r.target.as_str()) {
                anyhow::bail!("runs[{}] references unknown target: {}", i, r.target);
            }
            if !workload_names.contains(r.workload.as_str()) {
                anyhow::bail!("runs[{}] references unknown workload: {}", i, r.workload);
            }
        }
        for sw in &self.sweeps {
            if !workload_names.contains(sw.base.as_str()) {
                anyhow::bail!(
                    "sweep {:?} base workload not found: {}",
                    sw.name,
                    sw.base
                );
            }
            let sweep_targets: Vec<&String> = sw
                .targets
                .as_ref()
                .map(|v| v.iter().collect())
                .unwrap_or_else(|| sw.target.as_ref().map(|t| vec![t]).unwrap_or_default());
            for tn in sweep_targets {
                if !target_names.contains(tn.as_str()) {
                    anyhow::bail!("sweep {:?} references unknown target: {}", sw.name, tn);
                }
            }
        }
        Ok(())
    }

    pub fn target_by_name(&self, name: &str) -> Result<&Target> {
        self.targets
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow!("unknown target: {}", name))
    }

    pub fn workload_by_name(&self, name: &str) -> Result<&Workload> {
        self.workloads
            .iter()
            .find(|w| w.name == name)
            .ok_or_else(|| anyhow!("unknown workload: {}", name))
    }
}

// ---------------------------------------------------------------------------
// ByteSize deserialization
// ---------------------------------------------------------------------------

fn de_bytesize<'de, D>(d: D) -> Result<ByteSize, D::Error>
where
    D: Deserializer<'de>,
{
    let v = serde_yaml::Value::deserialize(d)?;
    parse_bytesize_value(&v).map_err(serde::de::Error::custom)
}

fn de_opt_bytesize<'de, D>(d: D) -> Result<Option<ByteSize>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<serde_yaml::Value>::deserialize(d)?;
    match v {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(v) => parse_bytesize_value(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn de_opt_bytesize_list<'de, D>(d: D) -> Result<Option<Vec<ByteSize>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Vec<serde_yaml::Value>>::deserialize(d)?;
    match v {
        None => Ok(None),
        Some(list) => list
            .iter()
            .map(parse_bytesize_value)
            .collect::<Result<Vec<_>>>()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn parse_bytesize_value(v: &serde_yaml::Value) -> Result<ByteSize> {
    match v {
        serde_yaml::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else if let Some(i) = n.as_i64() {
                if i < 0 {
                    Err(anyhow!("byte size must be >= 0, got {}", i))
                } else {
                    Ok(i as u64)
                }
            } else {
                Err(anyhow!("byte size number out of range: {:?}", n))
            }
        }
        serde_yaml::Value::String(s) => parse_bytesize_string(s),
        other => Err(anyhow!("cannot parse byte size from {:?}", other)),
    }
}

/// Parse a human-friendly byte string. 1024-based regardless of suffix
/// (matching Python's humanfriendly.parse_size with binary=True).
///
/// Accepts:
///   "1024" -> 1024
///   "1k", "1K", "1KB", "1KiB" -> 1024
///   "1m", "1MiB" -> 1024^2
///   "1g", "1GiB" -> 1024^3
///   "1t", "1TiB" -> 1024^4
pub fn parse_bytesize_string(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty byte size");
    }
    // Split numeric prefix from unit suffix.
    let split_idx = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.' && *c != '-')
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num_str, unit_str) = s.split_at(split_idx);
    let num_str = num_str.trim();
    let unit_str = unit_str.trim().to_lowercase();
    let n: f64 = num_str
        .parse()
        .map_err(|_| anyhow!("invalid byte size number: {:?}", num_str))?;
    if n.is_sign_negative() {
        anyhow::bail!("byte size must be non-negative");
    }
    let mult: u64 = match unit_str.as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024u64.pow(2),
        "g" | "gb" | "gib" => 1024u64.pow(3),
        "t" | "tb" | "tib" => 1024u64.pow(4),
        "p" | "pb" | "pib" => 1024u64.pow(5),
        other => anyhow::bail!("unknown byte-size unit: {:?}", other),
    };
    let bytes = (n * mult as f64).round() as u64;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bytesize_integer() {
        assert_eq!(parse_bytesize_string("1024").unwrap(), 1024);
        assert_eq!(parse_bytesize_string("0").unwrap(), 0);
    }

    #[test]
    fn parse_bytesize_kib() {
        for s in ["1k", "1K", "1Kb", "1kB", "1KiB", "1KIB"] {
            assert_eq!(parse_bytesize_string(s).unwrap(), 1024, "{}", s);
        }
    }

    #[test]
    fn parse_bytesize_mib() {
        assert_eq!(parse_bytesize_string("1MiB").unwrap(), 1024 * 1024);
        assert_eq!(parse_bytesize_string("256MiB").unwrap(), 256 * 1024 * 1024);
        assert_eq!(parse_bytesize_string("4m").unwrap(), 4 * 1024 * 1024);
    }

    #[test]
    fn parse_bytesize_gib_tib() {
        assert_eq!(parse_bytesize_string("1g").unwrap(), 1024u64.pow(3));
        assert_eq!(parse_bytesize_string("4TiB").unwrap(), 4 * 1024u64.pow(4));
    }

    #[test]
    fn parse_bytesize_rejects_garbage() {
        assert!(parse_bytesize_string("abc").is_err());
        assert!(parse_bytesize_string("1XB").is_err());
        assert!(parse_bytesize_string("-1k").is_err());
        assert!(parse_bytesize_string("").is_err());
    }

    #[test]
    fn engine_default_is_elbencho() {
        assert_eq!(Engine::default(), Engine::Elbencho);
    }

    #[test]
    fn client_host_default_is_localhost() {
        let c = ClientHost::default();
        assert_eq!(c.host, "localhost");
        assert_eq!(c.ssh_port, 22);
        assert_eq!(c.service_port, 1611);
        assert_eq!(c.elbencho_path, "elbencho");
    }

    #[test]
    fn target_validate_rejects_absolute_dataset_subdir() {
        let t = Target::Posix(PosixTarget {
            name: "t".into(),
            mount_path: "/mnt".into(),
            dataset_subdir: "/bench".into(),
            cleanup: false,
        });
        assert!(t.validate().is_err());
    }

    #[test]
    fn target_validate_rejects_dotdot_in_dataset_subdir() {
        let t = Target::Posix(PosixTarget {
            name: "t".into(),
            mount_path: "/mnt".into(),
            dataset_subdir: "../escape".into(),
            cleanup: false,
        });
        assert!(t.validate().is_err());
    }

    #[test]
    fn s3_target_rejects_inline_credentials() {
        let t = Target::S3(S3Target {
            name: "s3".into(),
            endpoint: "https://s3".into(),
            bucket: "b".into(),
            region: None,
            credentials_ref: "AKIA:inline".into(),
            addressing: "path".into(),
        });
        assert!(t.validate().is_err());
    }

    #[test]
    fn workload_requires_duration_or_dataset() {
        let w = Workload {
            name: "w".into(),
            pattern: "seq".into(),
            rw_mix_pct_read: 100,
            block_size: 4096,
            threads_per_client: 1,
            io_depth: 1,
            direct_io: false,
            sync_after_write: false,
            drop_caches_before: false,
            duration_s: None,
            dataset_size: None,
            file_size: None,
            file_count: None,
            s3_multipart_size: None,
            s3_object_prefix: None,
            extra_flags: vec![],
        };
        assert!(w.validate().is_err());
    }

    #[test]
    fn workload_with_file_size_is_valid() {
        let w = Workload {
            name: "w".into(),
            pattern: "seq".into(),
            rw_mix_pct_read: 100,
            block_size: 4096,
            threads_per_client: 1,
            io_depth: 1,
            direct_io: false,
            sync_after_write: false,
            drop_caches_before: false,
            duration_s: None,
            dataset_size: None,
            file_size: Some(4096),
            file_count: None,
            s3_multipart_size: None,
            s3_object_prefix: None,
            extra_flags: vec![],
        };
        assert!(w.validate().is_ok());
    }

    #[test]
    fn spec_hash_is_deterministic() {
        let target = Target::Posix(PosixTarget {
            name: "t".into(),
            mount_path: "/mnt".into(),
            dataset_subdir: "bench".into(),
            cleanup: false,
        });
        let workload = Workload {
            name: "w".into(),
            pattern: "seq".into(),
            rw_mix_pct_read: 100,
            block_size: 4096,
            threads_per_client: 1,
            io_depth: 1,
            direct_io: false,
            sync_after_write: false,
            drop_caches_before: false,
            duration_s: None,
            dataset_size: None,
            file_size: Some(4096),
            file_count: None,
            s3_multipart_size: None,
            s3_object_prefix: None,
            extra_flags: vec![],
        };
        let clients = vec![ClientHost::default()];
        let a = RunSpec::make_spec_hash(&target, &workload, &clients);
        let b = RunSpec::make_spec_hash(&target, &workload, &clients);
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));
        assert_eq!(a.len(), 7 + 64);
    }
}
