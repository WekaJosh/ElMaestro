//! Pre-flight validation.
//!
//! Verifies that everything needed to run a benchmark is in place:
//!   - the local machine has the engine binary
//!   - if S3 targets are configured, the local engine has S3 compiled in
//!   - every worker is reachable over SSH
//!   - every worker has the engine binary at `clients[i].elbencho_path`
//!   - every worker's engine has S3 (when needed)
//!   - the POSIX target mount path is writable on every worker
//!
//! Used both as a standalone subcommand (`elmaestro check <config>`) and
//! invoked automatically before `elmaestro run` (skip with --no-check).

use anyhow::Result;
use std::process::Command;
use std::time::Duration;

use crate::backends::get_backend;
use crate::config::{ClientHost, RunPlan, Target};
use crate::engine::ssh::SshRunner;

#[derive(Debug, Clone)]
pub struct LocalCheck {
    pub binary: String,
    pub present: bool,
    pub version: Option<String>,
    pub features: Vec<String>,
    pub s3_capable: bool,
}

#[derive(Debug, Clone)]
pub struct ClientCheckRow {
    pub host: String,
    pub ssh_ok: bool,
    pub ssh_error: Option<String>,
    pub binary: String,
    pub binary_present: bool,
    pub version: Option<String>,
    pub features: Vec<String>,
    pub s3_capable: bool,
    /// `Some(true)` if writable, `Some(false)` if not, `None` if untested
    /// (target wasn't POSIX or SSH check failed first).
    pub mount_writable: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub engine: String,
    pub needs_s3: bool,
    pub local: LocalCheck,
    pub clients: Vec<ClientCheckRow>,
    /// Non-empty means the run cannot proceed. Each string is a human-
    /// readable reason.
    pub fatal: Vec<String>,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.fatal.is_empty()
    }

    /// Render to stdout as a small formatted report.
    pub fn print(&self) {
        println!("Validation report ({})", self.engine);
        println!();
        let s3_marker = if self.needs_s3 { "  S3: required" } else { "  S3: not required" };
        println!("{}", s3_marker);
        println!();
        println!("Local engine:");
        if self.local.present {
            let s3 = if self.needs_s3 {
                if self.local.s3_capable { "✓ S3" } else { "✗ NO S3" }
            } else {
                ""
            };
            println!(
                "  ✓ {} {} {}",
                self.local.binary,
                self.local.version.as_deref().unwrap_or("(unknown)"),
                s3
            );
        } else {
            println!("  ✗ {} not found", self.local.binary);
        }
        println!();
        if !self.clients.is_empty() {
            println!("Workers:");
            println!(
                "  {:<28}  {:<6}  {:<22}  {:<6}  {}",
                "host", "ssh", "engine", "s3", "mount"
            );
            for c in &self.clients {
                let ssh = if c.ssh_ok { "✓" } else { "✗" };
                let engine_str = if c.binary_present {
                    format!("{} {}", c.binary, c.version.as_deref().unwrap_or("(?)"))
                } else if c.ssh_ok {
                    format!("{} missing", c.binary)
                } else {
                    "—".to_string()
                };
                let s3 = if !self.needs_s3 {
                    "n/a".to_string()
                } else if !c.binary_present {
                    "—".to_string()
                } else if c.s3_capable {
                    "✓".to_string()
                } else {
                    "✗".to_string()
                };
                let mount = match c.mount_writable {
                    Some(true) => "writable".to_string(),
                    Some(false) => "✗ not writable".to_string(),
                    None => "—".to_string(),
                };
                println!(
                    "  {:<28}  {:<6}  {:<22}  {:<6}  {}",
                    c.host, ssh, engine_str, s3, mount
                );
                if !c.ssh_ok {
                    if let Some(err) = &c.ssh_error {
                        println!("    ssh error: {}", err.trim());
                    }
                }
            }
            println!();
        }
        if self.ok() {
            println!("✓ Ready to run.");
        } else {
            println!("✗ Cannot proceed:");
            for line in &self.fatal {
                println!("  - {}", line);
            }
        }
    }
}

fn is_localhost_only(clients: &[ClientHost]) -> bool {
    if clients.len() != 1 {
        return false;
    }
    matches!(
        clients[0].host.as_str(),
        "localhost" | "127.0.0.1" | "::1" | ""
    )
}

fn plan_needs_s3(plan: &RunPlan) -> bool {
    plan.targets.iter().any(|t| matches!(t, Target::S3(_)))
}

fn posix_mount_path(plan: &RunPlan) -> Option<std::path::PathBuf> {
    // Use the first POSIX target's mount path as a representative for the
    // writable-check. If there's no POSIX target, skip the check.
    for t in &plan.targets {
        if let Target::Posix(p) = t {
            return Some(p.mount_path.clone());
        }
    }
    None
}

/// Run all the pre-flight checks for a plan. Synchronous wrapper around an
/// internal tokio runtime so the CLI doesn't have to be async-aware.
pub fn run_check(plan: &RunPlan) -> Result<CheckReport> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(runtime.block_on(run_check_async(plan)))
}

async fn run_check_async(plan: &RunPlan) -> CheckReport {
    let backend = get_backend(plan.engine);
    let needs_s3 = plan_needs_s3(plan);

    // Coordinator engine check. With a jump host configured the bastion
    // is the coordinator (the engine master runs there), so the check
    // targets the bastion and the LOCAL machine needs no engine binary
    // at all.
    let local_binary = plan
        .clients
        .first()
        .map(|c| c.elbencho_path.clone())
        .unwrap_or_else(|| backend.name().to_string());
    let bastion = plan
        .clients
        .first()
        .and_then(|c| c.ssh_jump.as_deref())
        .map(str::trim)
        .filter(|j| !j.is_empty())
        .map(|j| crate::engine::ssh::bastion_client(j, plan.clients.first().unwrap()));
    let local = match &bastion {
        Some(b) => check_coordinator_on_bastion(b, &local_binary).await,
        None => check_local(&local_binary, &*backend),
    };

    // Collect fatal reasons as we go.
    let mut fatal: Vec<String> = Vec::new();
    if !local.present {
        match &bastion {
            Some(b) => fatal.push(format!(
                "{} not found at {:?} on jump host {} — the bastion acts as \
                 the coordinator and needs the engine binary installed",
                backend.name(),
                local_binary,
                b.host
            )),
            None => fatal.push(format!(
                "local {} binary not found at {:?}; install it or set clients[0].elbencho_path",
                backend.name(),
                local_binary
            )),
        }
    } else if needs_s3 && !local.s3_capable {
        fatal.push(format!(
            "local {} doesn't have S3 support compiled in (rebuild with S3_SUPPORT=1 \
             or use the breuner/elbencho Docker image)",
            backend.name()
        ));
    }

    // Worker checks. Skip if single-localhost (covered by local check).
    let mut clients_rows: Vec<ClientCheckRow> = Vec::new();
    if !is_localhost_only(&plan.clients) {
        let mount = posix_mount_path(plan);
        let mut tasks = Vec::with_capacity(plan.clients.len());
        for c in &plan.clients {
            let c = c.clone();
            let mount = mount.clone();
            tasks.push(tokio::spawn(async move {
                check_one_worker(&c, mount.as_deref()).await
            }));
        }
        for t in tasks {
            match t.await {
                Ok(row) => clients_rows.push(row),
                Err(e) => clients_rows.push(ClientCheckRow {
                    host: "?".into(),
                    ssh_ok: false,
                    ssh_error: Some(format!("worker check task panicked: {}", e)),
                    binary: String::new(),
                    binary_present: false,
                    version: None,
                    features: Vec::new(),
                    s3_capable: false,
                    mount_writable: None,
                }),
            }
        }
        for row in &clients_rows {
            if !row.ssh_ok {
                fatal.push(format!(
                    "ssh to {} failed: {}",
                    row.host,
                    row.ssh_error.as_deref().unwrap_or("(no detail)")
                ));
                continue;
            }
            if !row.binary_present {
                fatal.push(format!(
                    "{} not found on {} (expected at {})",
                    backend.name(),
                    row.host,
                    row.binary
                ));
                continue;
            }
            if needs_s3 && !row.s3_capable {
                fatal.push(format!(
                    "{} on {} doesn't have S3 support compiled in",
                    backend.name(),
                    row.host
                ));
            }
            if matches!(row.mount_writable, Some(false)) {
                fatal.push(format!(
                    "POSIX mount path not writable on {} (the user running ssh must be \
                     able to write to it)",
                    row.host
                ));
            }
        }
    }

    CheckReport {
        engine: backend.name().to_string(),
        needs_s3,
        local,
        clients: clients_rows,
        fatal,
    }
}

/// Coordinator engine check for jump mode: run `<binary> --version` on
/// the bastion over SSH and parse version + features from its output.
async fn check_coordinator_on_bastion(bastion: &ClientHost, binary: &str) -> LocalCheck {
    let label = format!("{} (on jump host {})", binary, bastion.host);
    let runner = SshRunner::new(bastion.clone());
    match runner
        .run(&[binary, "--version"], Some(Duration::from_secs(15)))
        .await
    {
        Ok(r) if r.ok() => {
            let raw = format!("{}{}", r.stdout, r.stderr);
            let features = parse_features(&raw);
            LocalCheck {
                binary: label,
                present: true,
                version: parse_version(&raw),
                s3_capable: features.iter().any(|f| f.eq_ignore_ascii_case("S3")),
                features,
            }
        }
        _ => LocalCheck {
            binary: label,
            present: false,
            version: None,
            features: Vec::new(),
            s3_capable: false,
        },
    }
}

fn check_local(binary: &str, backend: &dyn crate::backends::Backend) -> LocalCheck {
    // Quick presence check via `which`-equivalent: try to run --version. If
    // the OS can't spawn the binary, mark not present.
    match Command::new(binary).arg("--version").output() {
        Err(_) => LocalCheck {
            binary: binary.into(),
            present: false,
            version: None,
            features: Vec::new(),
            s3_capable: false,
        },
        Ok(_) => match backend.detect_version(binary) {
            Ok(v) => {
                let s3 = v.has("S3");
                LocalCheck {
                    binary: binary.into(),
                    present: true,
                    version: v.version,
                    features: v.features,
                    s3_capable: s3,
                }
            }
            Err(_) => LocalCheck {
                binary: binary.into(),
                present: false,
                version: None,
                features: Vec::new(),
                s3_capable: false,
            },
        },
    }
}

async fn check_one_worker(
    client: &ClientHost,
    mount_path: Option<&std::path::Path>,
) -> ClientCheckRow {
    let runner = SshRunner::new(client.clone());
    let mut row = ClientCheckRow {
        host: client.host.clone(),
        ssh_ok: false,
        ssh_error: None,
        binary: client.elbencho_path.clone(),
        binary_present: false,
        version: None,
        features: Vec::new(),
        s3_capable: false,
        mount_writable: None,
    };

    // SSH reachability via `true`.
    match runner.run(&["true"], Some(Duration::from_secs(15))).await {
        Ok(r) if r.ok() => {
            row.ssh_ok = true;
        }
        Ok(r) => {
            row.ssh_error = Some(format!(
                "exit={} stderr={}",
                r.exit_status,
                r.stderr.lines().next().unwrap_or("")
            ));
            return row;
        }
        Err(e) => {
            row.ssh_error = Some(format!("{}", e));
            return row;
        }
    }

    // Engine version.
    let version_cmd = [client.elbencho_path.as_str(), "--version"];
    match runner.run(&version_cmd, Some(Duration::from_secs(10))).await {
        Ok(r) if r.ok() => {
            let raw = format!("{}{}", r.stdout, r.stderr);
            row.binary_present = true;
            row.version = parse_version(&raw);
            row.features = parse_features(&raw);
            row.s3_capable = row.features.iter().any(|f| f.eq_ignore_ascii_case("S3"));
        }
        Ok(_) | Err(_) => {
            row.binary_present = false;
        }
    }

    // Mount writability: `test -w <path>` returns 0 if writable.
    if row.binary_present {
        if let Some(p) = mount_path {
            let path_str = p.to_string_lossy().into_owned();
            let probe = format!("test -w {}", shlex_quote(&path_str));
            match runner
                .run(&["sh", "-c", &probe], Some(Duration::from_secs(10)))
                .await
            {
                Ok(r) => row.mount_writable = Some(r.ok()),
                Err(_) => row.mount_writable = Some(false),
            }
        }
    }
    row
}

pub(crate) fn parse_version(raw: &str) -> Option<String> {
    // Matches both elbencho ("version: 3.1.3") and fio ("fio-3.36") shapes.
    let re_elb =
        regex::Regex::new(r"(?i)version[:\s]+v?(\d+\.\d+(?:[.\-]\d+)?[^\s]*)").unwrap();
    if let Some(m) = re_elb.captures(raw).and_then(|c| c.get(1)) {
        return Some(m.as_str().to_string());
    }
    let re_fio = regex::Regex::new(r"fio[-\s]+(\d+\.\d+(?:\.\d+)?)").unwrap();
    re_fio
        .captures(raw)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub(crate) fn parse_features(raw: &str) -> Vec<String> {
    // Real elbencho prints features lowercase ("s3 syncfs syscallh"). Match
    // case-insensitively but emit the canonical capitalized name so callers
    // can do `features.contains(&"S3")` regardless of build output.
    let mut features = Vec::new();
    for feat in ["S3", "CUDA", "CUFILE"] {
        let re = regex::Regex::new(&format!(r"(?i)\b{}\b", feat)).unwrap();
        if re.is_match(raw) {
            features.push(feat.into());
        }
    }
    features
}

fn shlex_quote(s: &str) -> String {
    shlex::try_quote(s).map(|c| c.into_owned()).unwrap_or_else(|_| s.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_matches_elbencho() {
        let raw = "elbencho\n * Version: 3.1-1\n * Built with: S3";
        assert_eq!(parse_version(raw).as_deref(), Some("3.1-1"));
    }

    #[test]
    fn parse_version_matches_fio() {
        let raw = "fio-3.36\n";
        assert_eq!(parse_version(raw).as_deref(), Some("3.36"));
    }

    #[test]
    fn parse_features_picks_s3() {
        let raw = "Included optional build features: backtrace corebind libaio libnuma mimalloc ncurses s3 syncfs syscallh";
        let feats = parse_features(raw);
        assert!(feats.iter().any(|f| f == "S3"));
    }

    #[test]
    fn parse_features_no_s3() {
        let raw = "Included optional build features: backtrace corebind libaio";
        let feats = parse_features(raw);
        assert!(!feats.iter().any(|f| f == "S3"));
    }

    #[test]
    fn plan_needs_s3_picks_up_s3_target() {
        use crate::config::{Engine, PosixTarget, S3Target};
        let plan = RunPlan {
            version: 1,
            engine: Engine::Elbencho,
            output_dir: ".".into(),
            clients: vec![ClientHost::default()],
            targets: vec![
                Target::Posix(PosixTarget {
                    name: "a".into(),
                    mount_path: "/mnt".into(),
                    dataset_subdir: "bench".into(),
                    cleanup: false,
                }),
                Target::S3(S3Target {
                    name: "b".into(),
                    endpoint: "https://s3".into(),
                    bucket: "x".into(),
                    region: None,
                    credentials_ref: "env:X".into(),
                    addressing: "path".into(),
                }),
            ],
            workloads: Vec::new(),
            runs: Vec::new(),
            sweeps: Vec::new(),
        };
        assert!(plan_needs_s3(&plan));
    }
}
