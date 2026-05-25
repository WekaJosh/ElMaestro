//! YAML loader with placeholder substitution.
//!
//! Mirrors python-legacy/src/elbencho_harness/config/loader.py. Three
//! placeholders are expanded textually before YAML parses:
//!
//!   ${CONFIG_DIR}   absolute directory of the YAML file being loaded
//!   ${HOME}         user's home directory
//!   $ENV{NAME}      environment variable, empty string if unset
//!
//! Textual substitution means the placeholders work in any string-valued
//! field (paths, hosts, prefixes, credentials_refs).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;

use super::RunPlan;

static ENV_RE: Lazy<Regex> = Lazy::new(|| {
    // $ENV{NAME} where NAME is a typical env-var identifier.
    Regex::new(r"\$ENV\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("ENV regex compiles")
});

/// Replace placeholders in a raw YAML string.
pub fn expand_placeholders(raw: &str, config_dir: &Path) -> String {
    let mut out = raw.replace("${CONFIG_DIR}", &config_dir.display().to_string());
    let home = home_dir();
    out = out.replace("${HOME}", &home.display().to_string());
    out = ENV_RE
        .replace_all(&out, |caps: &regex::Captures<'_>| {
            std::env::var(&caps[1]).unwrap_or_default()
        })
        .into_owned();
    out
}

fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h)
    } else {
        PathBuf::from("/")
    }
}

/// Load a RunPlan from a YAML file. Expands placeholders, parses, validates.
pub fn load(path: &Path) -> Result<RunPlan> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config: {}", path.display()))?;
    let config_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));
    let expanded = expand_placeholders(&raw, &config_dir);
    let plan: RunPlan = serde_yaml::from_str(&expanded)
        .with_context(|| format!("parsing YAML in {}", path.display()))?;
    plan.validate()
        .with_context(|| format!("validating {}", path.display()))?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_dir_substitution() {
        let dir = Path::new("/tmp/x");
        let out = expand_placeholders("path: ${CONFIG_DIR}/file.sh", dir);
        assert_eq!(out, "path: /tmp/x/file.sh");
    }

    #[test]
    fn home_substitution() {
        std::env::set_var("HOME", "/var/empty/test");
        let out = expand_placeholders("key: ${HOME}/.ssh/id", Path::new("/"));
        assert_eq!(out, "key: /var/empty/test/.ssh/id");
    }

    #[test]
    fn env_substitution() {
        std::env::set_var("BENCH_TEST_VAR", "the-value");
        let out = expand_placeholders("v: $ENV{BENCH_TEST_VAR}", Path::new("/"));
        assert_eq!(out, "v: the-value");
    }

    #[test]
    fn env_missing_becomes_empty() {
        std::env::remove_var("DEFINITELY_UNSET_12345");
        let out = expand_placeholders("v: $ENV{DEFINITELY_UNSET_12345}", Path::new("/"));
        assert_eq!(out, "v: ");
    }

    #[test]
    fn load_minimal_yaml() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.yaml");
        std::fs::write(
            &path,
            r#"
version: 1
clients:
  - host: localhost
targets:
  - name: t
    kind: posix
    mount_path: /mnt
workloads:
  - name: w
    block_size: 4096
    file_size: 4096
runs:
  - target: t
    workload: w
"#,
        )
        .unwrap();
        let plan = load(&path).unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.workloads.len(), 1);
        assert_eq!(plan.runs.len(), 1);
        assert_eq!(plan.engine.to_string(), "elbencho");
    }

    #[test]
    fn load_with_config_dir_placeholder() {
        let tmp = TempDir::new().unwrap();
        let fixture_path = tmp.path().join("fake.sh");
        std::fs::write(&fixture_path, "#!/bin/sh\nexit 0\n").unwrap();
        let path = tmp.path().join("c.yaml");
        std::fs::write(
            &path,
            r#"
version: 1
clients:
  - host: localhost
    elbencho_path: ${CONFIG_DIR}/fake.sh
targets:
  - name: t
    kind: posix
    mount_path: /mnt
workloads:
  - name: w
    block_size: 4096
    file_size: 4096
runs:
  - target: t
    workload: w
"#,
        )
        .unwrap();
        let plan = load(&path).unwrap();
        assert!(plan.clients[0].elbencho_path.contains("fake.sh"));
        assert!(plan.clients[0].elbencho_path.starts_with('/'));
    }

    #[test]
    fn load_rejects_unknown_target() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.yaml");
        std::fs::write(
            &path,
            r#"
version: 1
clients: [{host: localhost}]
targets:
  - name: t1
    kind: posix
    mount_path: /mnt
workloads:
  - name: w
    block_size: 4096
    file_size: 4096
runs:
  - target: missing
    workload: w
"#,
        )
        .unwrap();
        let err = load(&path).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("unknown target") || msg.contains("missing"));
    }
}
