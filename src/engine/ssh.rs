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
        // Skip the hop when this client IS the jump host: `ssh -J bastion
        // bastion` tunnels to the bastion through itself, which fails
        // with ssh exit 255. The bastion is directly reachable by
        // definition, so connect straight to it.
        if !j.is_empty() && jump_host_part(j) != client.host {
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

/// Parse an ssh -J style jump spec — `host`, `user@host`, or
/// `user@host:port` — into a pseudo-ClientHost for talking to the bastion
/// DIRECTLY (the bastion is the coordinator when a jump host is set; the
/// engine master runs there). Key and engine path are inherited from the
/// template worker client. Password auth is intentionally NOT inherited:
/// the bastion must use keys/agent (sshpass answers only one prompt, and
/// that one belongs to the workers).
pub fn bastion_client(jump: &str, template: &ClientHost) -> ClientHost {
    let j = jump.trim();
    let (user_part, hostport) = match j.split_once('@') {
        Some((u, rest)) => (Some(u.to_string()), rest),
        None => (None, j),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(pn) => (h.to_string(), pn),
            // Not a port number (e.g. a bare IPv6-ish string) — treat the
            // whole thing as the host.
            Err(_) => (hostport.to_string(), 22),
        },
        None => (hostport.to_string(), 22),
    };
    ClientHost {
        host,
        ssh_user: user_part.or_else(|| template.ssh_user.clone()),
        ssh_port: port,
        ssh_key: template.ssh_key.clone(),
        ssh_jump: None,
        ssh_password: None,
        elbencho_path: template.elbencho_path.clone(),
        service_port: template.service_port,
    }
}

/// Just the host portion of a jump spec: strips `user@` and a trailing
/// numeric `:port`. Used to detect the jump-host-is-also-a-worker case.
fn jump_host_part(jump: &str) -> String {
    let j = jump.trim();
    let hostport = j.split_once('@').map(|(_, rest)| rest).unwrap_or(j);
    match hostport.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h.to_string(),
        _ => hostport.to_string(),
    }
}

/// The ssh argv for a host, joined into one shell-quoted string, for
/// embedding in tar-pipe transfer pipelines.
fn ssh_cmd_string(client: &ClientHost) -> String {
    ssh_base_argv(client)
        .iter()
        .map(|s| {
            shlex::try_quote(s)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| s.clone())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Copy the CONTENTS of `local_dir` into `remote_dir` on the host
/// (created if missing) via a tar pipe — no scp/sftp dependency, one
/// round trip regardless of file count.
pub async fn push_dir(client: &ClientHost, local_dir: &std::path::Path, remote_dir: &str) -> Result<()> {
    let remote_cmd = format!(
        "mkdir -p {rd} && tar xf - -C {rd}",
        rd = shlex::try_quote(remote_dir).map(|c| c.into_owned()).unwrap_or_else(|_| remote_dir.into())
    );
    let pipeline = format!(
        "tar cf - -C {ld} . | {ssh} -- {rc}",
        ld = shlex::try_quote(&local_dir.to_string_lossy()).map(|c| c.into_owned()).unwrap_or_default(),
        ssh = ssh_cmd_string(client),
        rc = shlex::try_quote(&remote_cmd).map(|c| c.into_owned()).unwrap_or_default(),
    );
    run_pipeline(&pipeline, &client.host).await
}

/// Copy the CONTENTS of `remote_dir` on the host into `local_dir`
/// (created if missing) via a tar pipe.
pub async fn pull_dir(client: &ClientHost, remote_dir: &str, local_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(local_dir)
        .with_context(|| format!("creating {}", local_dir.display()))?;
    let remote_cmd = format!(
        "tar cf - -C {rd} .",
        rd = shlex::try_quote(remote_dir).map(|c| c.into_owned()).unwrap_or_else(|_| remote_dir.into())
    );
    let pipeline = format!(
        "{ssh} -- {rc} | tar xf - -C {ld}",
        ssh = ssh_cmd_string(client),
        rc = shlex::try_quote(&remote_cmd).map(|c| c.into_owned()).unwrap_or_default(),
        ld = shlex::try_quote(&local_dir.to_string_lossy()).map(|c| c.into_owned()).unwrap_or_default(),
    );
    run_pipeline(&pipeline, &client.host).await
}

async fn run_pipeline(pipeline: &str, host: &str) -> Result<()> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(pipeline)
        .output()
        .await
        .with_context(|| format!("running transfer pipeline for {}", host))?;
    if !out.status.success() {
        anyhow::bail!(
            "transfer to/from {} failed (exit={}): {}",
            host,
            out.status.code().unwrap_or(-1),
            truncate(&String::from_utf8_lossy(&out.stderr), 300)
        );
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
    fn jump_host_that_is_also_a_worker_connects_directly() {
        // `ssh -J bastion bastion` tunnels through itself and dies with
        // exit 255. When the client IS the jump host, drop -J entirely.
        for jump in ["10.0.0.1", "admin@10.0.0.1", "admin@10.0.0.1:2222"] {
            let argv = ssh_base_argv(&ClientHost {
                host: "10.0.0.1".into(),
                ssh_jump: Some(jump.into()),
                ..Default::default()
            });
            assert!(
                !argv.iter().any(|a| a == "-J"),
                "expected no -J for jump={:?}, got {:?}",
                jump,
                argv
            );
        }
        // Other workers still hop through the bastion.
        let argv = ssh_base_argv(&ClientHost {
            host: "10.0.0.2".into(),
            ssh_jump: Some("admin@10.0.0.1:2222".into()),
            ..Default::default()
        });
        assert!(argv.iter().any(|a| a == "-J"));
    }

    #[test]
    fn bastion_client_parses_all_jump_forms() {
        let template = ClientHost {
            host: "worker".into(),
            ssh_user: Some("bench".into()),
            ssh_key: Some("/k".into()),
            ssh_password: Some("pw".into()),
            elbencho_path: "fio".into(),
            ..Default::default()
        };
        // bare host
        let b = bastion_client("bastion", &template);
        assert_eq!(b.host, "bastion");
        assert_eq!(b.ssh_port, 22);
        // user falls back to the template's
        assert_eq!(b.ssh_user.as_deref(), Some("bench"));
        // key + engine path inherited; password NEVER inherited
        assert_eq!(b.ssh_key.as_deref(), Some(std::path::Path::new("/k")));
        assert_eq!(b.elbencho_path, "fio");
        assert!(b.ssh_password.is_none());
        assert!(b.ssh_jump.is_none());

        // user@host
        let b = bastion_client("admin@bastion", &template);
        assert_eq!(b.host, "bastion");
        assert_eq!(b.ssh_user.as_deref(), Some("admin"));

        // user@host:port
        let b = bastion_client("admin@bastion:2222", &template);
        assert_eq!(b.host, "bastion");
        assert_eq!(b.ssh_user.as_deref(), Some("admin"));
        assert_eq!(b.ssh_port, 2222);

        // trailing non-numeric colon segment is part of the host
        let b = bastion_client("weird:host", &template);
        assert_eq!(b.host, "weird:host");
        assert_eq!(b.ssh_port, 22);
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
