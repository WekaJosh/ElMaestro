//! Engine service-mode lifecycle.
//!
//! Brings up `elbencho --service` / `fio --server` on each ClientHost in
//! parallel, probes the port until it accepts connections, yields the list
//! of endpoints for the master to use in --hosts / --client. Tears down on
//! drop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::ssh::SshRunner;
use crate::backends::Backend;
use crate::config::ClientHost;

#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub host: String,
    pub port: u16,
    pub engine_version: Option<String>,
}

impl ServiceEndpoint {
    pub fn as_hosts_arg(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Format a list of endpoints for elbencho's --hosts flag (host:port,host:port).
/// The fio backend translates this to --client=host,port internally.
pub fn hosts_arg(endpoints: &[ServiceEndpoint]) -> String {
    endpoints
        .iter()
        .map(|e| e.as_hosts_arg())
        .collect::<Vec<_>>()
        .join(",")
}

/// RAII handle: holds active SshRunners (with their bg processes). On drop,
/// each runner's close() runs (via the supplied tokio runtime).
pub struct ServicesGuard {
    runners: Vec<Arc<SshRunner>>,
}

impl ServicesGuard {
    pub async fn shutdown(self) {
        for r in self.runners.into_iter() {
            r.close().await;
        }
    }
}

/// Bring up service-mode processes on every client in parallel.
///
/// `backend.service_command` returns the argv for each client's service
/// start. Returns the connected endpoints + a guard that the caller must
/// `shutdown().await` after the run completes.
/// How the coordinator verifies a worker's service port is accepting
/// connections after start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    /// TCP-connect from this process. Correct when the master runs here
    /// and therefore must be able to reach the port itself.
    FromCoordinator,
    /// Check on the worker itself over SSH (bash /dev/tcp against
    /// 127.0.0.1). Used in jump-host mode: the laptop can't reach the
    /// service ports directly — only the bastion can — so probing
    /// locally would always fail even when the service is healthy.
    OnWorker,
}

pub async fn bring_up(
    backend: &dyn Backend,
    clients: &[ClientHost],
    connect_timeout: Duration,
    probe_mode: ProbeMode,
) -> Result<(Vec<ServiceEndpoint>, ServicesGuard)> {
    // 1. SSH health check across all clients in parallel.
    let mut tasks = Vec::with_capacity(clients.len());
    for c in clients {
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            let runner = Arc::new(SshRunner::new(c.clone()));
            let r = runner
                .run(&["true"], Some(connect_timeout))
                .await
                .map_err(|e| anyhow!("ssh check failed for {}: {}", c.host, e))?;
            if !r.ok() {
                return Err(anyhow!(
                    "ssh to {} returned exit={} stderr={}",
                    c.host,
                    r.exit_status,
                    truncate(&r.stderr, 200)
                ));
            }
            Result::<Arc<SshRunner>>::Ok(runner)
        }));
    }
    let mut runners: Vec<Arc<SshRunner>> = Vec::with_capacity(clients.len());
    for t in tasks {
        runners.push(t.await??);
    }

    // 2. Start service on each, probe each port. Run starts concurrently.
    let mut start_tasks = Vec::with_capacity(runners.len());
    for runner in &runners {
        let runner = runner.clone();
        let svc_cmd = backend.service_command(&runner.client);
        let port = runner.client.service_port;
        let host = runner.client.host.clone();
        start_tasks.push(tokio::spawn(async move {
            let svc_argv: Vec<&str> = svc_cmd.iter().map(|s| s.as_str()).collect();
            let bg = runner.start_background(&svc_argv).await?;
            let probe_result = match probe_mode {
                ProbeMode::FromCoordinator => {
                    wait_for_service(&host, port, 30, Duration::from_millis(500)).await
                }
                ProbeMode::OnWorker => wait_for_service_on_worker(&runner, port).await,
            };
            if let Err(probe_err) = probe_result {
                // The #1 cause is the service process dying instantly
                // (missing binary, bad flag, port already bound). Its
                // stdout/stderr landed in the remote log file — pull the
                // tail back so the user sees WHY instead of a bare
                // "never came up". The #2 cause is a firewall between
                // the coordinator and the worker; a clean log points
                // there.
                let tail_cmd = format!("tail -n 5 {} 2>/dev/null", bg.log_file);
                let log_tail = runner
                    .run(&["sh", "-c", &tail_cmd], Some(Duration::from_secs(5)))
                    .await
                    .ok()
                    .map(|r| r.stdout.trim().to_string())
                    .filter(|s| !s.is_empty());
                return Err(match log_tail {
                    Some(log) => anyhow!(
                        "{}; remote service log says: {} (service cmd: {})",
                        probe_err,
                        log,
                        svc_cmd.join(" ")
                    ),
                    None => anyhow!(
                        "{}; remote service log is empty — the service \
                         process likely started but the port is \
                         unreachable from this machine (firewall?). \
                         service cmd: {}",
                        probe_err,
                        svc_cmd.join(" ")
                    ),
                });
            }
            Ok::<ServiceEndpoint, anyhow::Error>(ServiceEndpoint {
                host,
                port,
                engine_version: None,
            })
        }));
    }
    let mut endpoints: Vec<ServiceEndpoint> = Vec::with_capacity(runners.len());
    for t in start_tasks {
        endpoints.push(t.await??);
    }

    Ok((endpoints, ServicesGuard { runners }))
}

/// Probe the service port from the worker itself: one SSH call running a
/// retry loop against 127.0.0.1 with bash's /dev/tcp (present on every
/// mainstream Linux). 30 × 0.5s, mirroring the coordinator-side probe.
async fn wait_for_service_on_worker(runner: &SshRunner, port: u16) -> Result<()> {
    let script = format!(
        "for i in $(seq 1 30); do \
           (exec 3<>/dev/tcp/127.0.0.1/{p}) 2>/dev/null && exit 0; \
           sleep 0.5; \
         done; exit 1",
        p = port
    );
    let r = runner
        .run(&["bash", "-c", &script], Some(Duration::from_secs(25)))
        .await?;
    if r.ok() {
        Ok(())
    } else {
        Err(anyhow!(
            "service never came up on {}:{} (probed from the worker itself)",
            runner.host(),
            port
        ))
    }
}

async fn wait_for_service(
    host: &str,
    port: u16,
    attempts: u32,
    interval: Duration,
) -> Result<()> {
    for _ in 0..attempts {
        if probe(host, port, Duration::from_secs(1)).await {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
    Err(anyhow!("service never came up on {}:{}", host, port))
}

async fn probe(host: &str, port: u16, t: Duration) -> bool {
    let addr = format!("{}:{}", host, port);
    matches!(
        timeout(t, TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_arg_joins_correctly() {
        let eps = vec![
            ServiceEndpoint {
                host: "h1".into(),
                port: 1611,
                engine_version: None,
            },
            ServiceEndpoint {
                host: "h2".into(),
                port: 1612,
                engine_version: None,
            },
        ];
        assert_eq!(hosts_arg(&eps), "h1:1611,h2:1612");
    }
}
