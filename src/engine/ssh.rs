//! SSH layer built on the system `ssh(1)` binary.
//!
//! Lifecycle:
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

/// True when the client authenticates with a password (non-empty
/// ssh_password). Drives both the argv shape and the SSHPASS env var.
fn uses_password(client: &ClientHost) -> bool {
    client
        .ssh_password
        .as_deref()
        .map(|p| !p.is_empty())
        .unwrap_or(false)
}

/// Build the base ssh argv for a ClientHost.
///
/// Honors ssh_port, ssh_user, ssh_key, ssh_jump, ssh_password. Key-auth
/// hosts get BatchMode=yes (never prompt). Password hosts are wrapped in
/// `sshpass -e` instead — BatchMode would disable password auth — with
/// the password delivered via the SSHPASS env var by SshRunner, never
/// argv, so it can't leak through `ps`.
pub fn ssh_base_argv(client: &ClientHost) -> Vec<String> {
    let mut argv: Vec<String> = if uses_password(client) {
        vec![
            "sshpass".into(),
            "-e".into(),
            "ssh".into(),
            "-o".into(),
            "NumberOfPasswordPrompts=1".into(),
        ]
    } else {
        vec!["ssh".into(), "-o".into(), "BatchMode=yes".into()]
    };
    argv.extend(
        [
            "-o",
            "ConnectTimeout=10",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ]
        .map(String::from),
    );
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
    if let Some(jump) = &client.ssh_jump {
        let j = jump.trim();
        if !j.is_empty() {
            argv.push("-J".into());
            argv.push(j.to_string());
        }
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
        if let Some(pw) = self.client.ssh_password.as_deref().filter(|p| !p.is_empty()) {
            // sshpass -e reads the password from SSHPASS. Env, not argv:
            // argv is visible to every user on the box via `ps`.
            command.env("SSHPASS", pw);
        }
        let fut = command.output();
        let out_res = match timeout {
            Some(d) => tokio::time::timeout(d, fut)
                .await
                .map_err(|_| SshError::Failed {
                    host: self.client.host.clone(),
                    message: format!("timeout running {:?}", cmd_str),
                })?,
            None => fut.await,
        };
        let out = out_res.map_err(|e| map_spawn_error(e, &base[0], &self.client.host))?;
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

/// Turn a process-spawn failure into something actionable. The big one:
/// a password-auth client needs `sshpass` on the coordinator, and a bare
/// "No such file or directory" doesn't tell the user that.
fn map_spawn_error(e: std::io::Error, binary: &str, host: &str) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound && binary == "sshpass" {
        return anyhow!(
            "ssh password auth for {} requires `sshpass` on this machine; \
             install it (apt/dnf install sshpass · brew install sshpass) \
             or switch the host to key auth",
            host
        );
    }
    anyhow!("spawning {}: {}", binary, e)
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
    fn base_argv_password_wraps_with_sshpass_and_drops_batchmode() {
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_password: Some("secret".into()),
            ..Default::default()
        });
        assert_eq!(argv[0], "sshpass");
        assert_eq!(argv[1], "-e");
        assert_eq!(argv[2], "ssh");
        // BatchMode would disable password auth.
        assert!(!argv.iter().any(|a| a == "BatchMode=yes"));
        assert!(argv.iter().any(|a| a == "NumberOfPasswordPrompts=1"));
        // The password itself must NEVER appear in argv (ps would show it).
        assert!(!argv.iter().any(|a| a.contains("secret")));
        // Common hardening flags stay.
        assert!(argv.iter().any(|a| a == "ConnectTimeout=10"));
    }

    #[test]
    fn base_argv_blank_password_means_key_auth() {
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_password: Some("   ".into()),
            ..Default::default()
        });
        // Whitespace-only is not a password... but trim happens at the
        // form layer; here only the empty string disables it. A literal
        // whitespace password is technically valid, so it wraps.
        assert_eq!(argv[0], "sshpass");

        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_password: Some("".into()),
            ..Default::default()
        });
        assert_eq!(argv[0], "ssh");
        assert!(argv.iter().any(|a| a == "BatchMode=yes"));
    }

    #[test]
    fn base_argv_jump_host_with_port_passes_through() {
        let argv = ssh_base_argv(&ClientHost {
            host: "internal".into(),
            ssh_jump: Some("user@bastion:2222".into()),
            ..Default::default()
        });
        let j_idx = argv.iter().position(|a| a == "-J").expect("-J flag");
        assert_eq!(argv[j_idx + 1], "user@bastion:2222");
    }

    #[test]
    fn base_argv_includes_jump_host_when_set() {
        let argv = ssh_base_argv(&ClientHost {
            host: "internal-worker".into(),
            ssh_user: Some("bench".into()),
            ssh_jump: Some("user@bastion.example.com".into()),
            ..Default::default()
        });
        let j_idx = argv.iter().position(|a| a == "-J").expect("ssh -J flag");
        assert_eq!(argv[j_idx + 1], "user@bastion.example.com");
        // The final host arg is the actual target, not the jump.
        assert_eq!(argv.last().unwrap(), "bench@internal-worker");
    }

    #[test]
    fn base_argv_omits_jump_host_when_blank() {
        let argv = ssh_base_argv(&ClientHost {
            host: "h".into(),
            ssh_jump: Some("   ".into()),
            ..Default::default()
        });
        assert!(!argv.iter().any(|a| a == "-J"));
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
