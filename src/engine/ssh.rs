//! SSH layer built on the system `ssh(1)` binary.
//!
//! Mirrors python-legacy/src/elbencho_harness/engine/ssh.py (v0.9, the
//! subprocess-based rewrite that dropped asyncssh). Same lifecycle:
//!   - start_background spawns a remote `nohup <cmd> > log 2>&1 &` with a
//!     PID file written to /tmp/elmaestro-<marker>.pid
//!   - close()/stop_background kills via `kill $(cat pid_file)` and cleans up

use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use crate::config::ClientHost;

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("ssh to {host}: {message}")]
    Failed { host: String, message: String },
}

#[derive(Debug, Clone)]
pub struct SshResult {
    pub exit_status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl SshResult {
    pub fn ok(&self) -> bool {
        self.exit_status == 0
    }
}

#[derive(Debug, Clone)]
pub struct BgProcess {
    pub marker: String,
    pub pid_file: String,
    pub log_file: String,
}

/// Build the base ssh argv for a ClientHost.
///
/// Honors ssh_port, ssh_user, ssh_key. Adds BatchMode=yes (never prompt for
/// password) and ConnectTimeout=10 (fail fast on dead hosts).
pub fn ssh_base_argv(client: &ClientHost) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    if client.ssh_port != 22 {
        argv.push("-p".into());
        argv.push(client.ssh_port.to_string());
    }
    if let Some(key) = &client.ssh_key {
        // shellexpand expands ~/, $VAR, etc.
        let expanded = shellexpand::tilde(&key.to_string_lossy()).into_owned();
        argv.push("-i".into());
        argv.push(expanded);
    }
    let host = match &client.ssh_user {
        Some(user) => format!("{}@{}", user, client.host),
        None => client.host.clone(),
    };
    argv.push(host);
    argv
}

/// One client's ssh runner. Tracks any background processes it started so
/// they can be cleanly killed on close().
pub struct SshRunner {
    pub client: ClientHost,
    bg_procs: Mutex<Vec<BgProcess>>,
}

impl SshRunner {
    pub fn new(client: ClientHost) -> Self {
        Self {
            client,
            bg_procs: Mutex::new(Vec::new()),
        }
    }

    pub fn host(&self) -> &str {
        &self.client.host
    }

    /// Run a one-shot remote command. Returns captured stdout/stderr/exit.
    pub async fn run(&self, argv: &[&str], timeout: Option<std::time::Duration>) -> Result<SshResult> {
        let cmd_str = argv
            .iter()
            .map(|s| shlex::try_quote(s).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        let mut base = ssh_base_argv(&self.client);
        base.push("--".into());
        base.push(cmd_str.clone());

        let mut command = Command::new(&base[0]);
        command.args(&base[1..]);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        let fut = command.output();
        let out = match timeout {
            Some(d) => tokio::time::timeout(d, fut)
                .await
                .map_err(|_| SshError::Failed {
                    host: self.client.host.clone(),
                    message: format!("timeout running {:?}", cmd_str),
                })??,
            None => fut.await?,
        };
        Ok(SshResult {
            exit_status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Start a long-running remote process. Writes a PID file so the
    /// matching stop_background can kill it cleanly later.
    pub async fn start_background(&self, argv: &[&str]) -> Result<BgProcess> {
        let cmd_str = argv
            .iter()
            .map(|s| shlex::try_quote(s).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        // Marker derived from random hex to avoid collisions.
        let marker: String = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{:016x}{:08x}", nanos as u64, rand_u32())
        };
        let pid_file = format!("/tmp/elmaestro-{}.pid", marker);
        let log_file = format!("/tmp/elmaestro-{}.log", marker);
        let wrap = format!(
            "nohup {} > {} 2>&1 < /dev/null & echo $! > {}",
            cmd_str, log_file, pid_file
        );
        let r = self
            .run(&["sh", "-c", &wrap], Some(std::time::Duration::from_secs(15)))
            .await?;
        if !r.ok() {
            anyhow::bail!(
                "failed to start background process on {}: exit={} stderr={}",
                self.client.host,
                r.exit_status,
                truncate(&r.stderr, 200)
            );
        }
        let bg = BgProcess {
            marker,
            pid_file,
            log_file,
        };
        self.bg_procs.lock().unwrap().push(bg.clone());
        Ok(bg)
    }

    /// Kill every bg process previously started. Best-effort, never raises.
    pub async fn stop_background(&self) {
        let drained: Vec<BgProcess> = {
            let mut g = self.bg_procs.lock().unwrap();
            std::mem::take(&mut *g)
        };
        for bg in drained {
            let cleanup = format!(
                "if [ -f {pid} ]; then kill $(cat {pid}) 2>/dev/null || true; fi; rm -f {pid} {log}",
                pid = bg.pid_file,
                log = bg.log_file,
            );
            let _ = self
                .run(
                    &["sh", "-c", &cleanup],
                    Some(std::time::Duration::from_secs(10)),
                )
                .await;
        }
    }

    pub async fn close(&self) {
        self.stop_background().await;
    }
}

/// Connect-check a client by running `true` over ssh. Errors propagate so
/// the coordinator can fail-fast on dead clients before doing real work.
pub async fn health_check(client: &ClientHost) -> Result<()> {
    let runner = SshRunner::new(client.clone());
    let r = runner
        .run(
            &["true"],
            Some(std::time::Duration::from_secs(15)),
        )
        .await
        .with_context(|| format!("ssh health check for {}", client.host))?;
    if !r.ok() {
        return Err(anyhow!(
            "ssh to {} failed: exit={} stderr={}",
            client.host,
            r.exit_status,
            truncate(&r.stderr, 200)
        ));
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Tiny stdlib-only PRNG seed source. Enough randomness for bg marker uniqueness;
/// not for any cryptographic purpose.
fn rand_u32() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mix = n.wrapping_mul(0x9E37_79B1).rotate_left(13);
    let pid = std::process::id();
    mix ^ pid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_argv_minimal() {
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ..Default::default()
        });
        assert_eq!(argv[0], "ssh");
        assert!(argv.iter().any(|a| a == "BatchMode=yes"));
        assert!(argv.iter().any(|a| a == "ConnectTimeout=10"));
        assert_eq!(argv.last().unwrap(), "h");
        // No -p when port is the default 22.
        assert!(!argv.iter().any(|a| a == "-p"));
    }

    #[test]
    fn base_argv_with_user_port_key() {
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_user: Some("bench".into()),
            ssh_port: 2222,
            ssh_key: Some("/tmp/key".into()),
            ..Default::default()
        });
        assert!(argv.iter().any(|a| a == "-p"));
        assert!(argv.iter().any(|a| a == "2222"));
        assert!(argv.iter().any(|a| a == "-i"));
        assert!(argv.iter().any(|a| a == "/tmp/key"));
        assert!(argv.iter().any(|a| a == "bench@h"));
    }

    #[test]
    fn base_argv_expands_tilde_in_key() {
        std::env::set_var("HOME", "/tmp/fakehome");
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_user: Some("u".into()),
            ssh_key: Some("~/.ssh/id".into()),
            ..Default::default()
        });
        let i_idx = argv.iter().position(|a| a == "-i").unwrap();
        let key = &argv[i_idx + 1];
        assert!(!key.starts_with('~'), "tilde must be expanded: {}", key);
    }
}
