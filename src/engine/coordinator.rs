//! Backend-agnostic coordinator.
//!
//! Two execution paths:
//!   - `run_locally`: single-host, no SSH. Used when clients == [localhost].
//!   - `run_fanout`:  multi-host. Starts engine service mode over SSH, runs
//!                    the master locally with --hosts.
//!
//! Top-level `run()` dispatches based on the spec's client list.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

use crate::backends::Backend;
use crate::config::{ClientHost, RunSpec, Target};
use crate::results::schema::{
    ClientInfo, EngineArtifactRefs, PhaseResult, Result as RunResult, TargetSnapshot,
    WorkloadSnapshot, SCHEMA_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("{0}")]
    Other(String),
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

fn ensure_posix_dataset_dir(spec: &RunSpec) -> Result<()> {
    if let Target::Posix(t) = &spec.target {
        let dataset = t.mount_path.join(&t.dataset_subdir);
        std::fs::create_dir_all(&dataset)
            .with_context(|| format!("creating dataset dir {}", dataset.display()))?;
    }
    Ok(())
}

fn local_path(spec: &RunSpec) -> String {
    spec.clients
        .first()
        .map(|c| c.elbencho_path.clone())
        .unwrap_or_else(|| "elbencho".into())
}

/// The bastion pseudo-client when a jump host is configured. Presence
/// flips the fan-out into jump mode: the bastion IS the coordinator —
/// engine version checks, service-port probes, and the master process
/// all happen there, and the local machine needs neither the engine
/// binary nor TCP reachability to the workers.
fn jump_bastion(spec: &RunSpec) -> Option<ClientHost> {
    let first = spec.clients.first()?;
    let jump = first.ssh_jump.as_deref()?.trim();
    if jump.is_empty() {
        return None;
    }
    Some(crate::engine::ssh::bastion_client(jump, first))
}

/// True when the spec's measured workload needs files to exist before it
/// runs (any reads in the mix) AND the target is file-based with sizing
/// fully specified. False for pure writes (the measure phase IS the
/// layout) and for S3 (object semantics, no pre-create needed).
fn needs_layout(spec: &RunSpec) -> bool {
    if spec.workload.rw_mix_pct_read == 0 {
        return false;
    }
    if !matches!(spec.target, Target::Posix(_)) {
        return false;
    }
    spec.workload.file_count.is_some() && spec.workload.file_size.is_some()
}

/// Synthesize a write-only spec used to lay out the dataset before the
/// measured workload runs. Fixed parameters regardless of what the
/// measure phase uses, so layout time is predictable and the dataset is
/// always written linearly with direct IO:
///   - pattern: seq
///   - block_size: 1 MiB
///   - direct_io: true
///   - rw_mix_pct_read: 0 (pure write)
///   - duration_s: None (run until N files of S bytes each are written)
///
/// Inherits threads_per_client / file_count / file_size from the user's
/// spec so the resulting layout matches what the measure phase reads.
fn layout_spec_for(spec: &RunSpec) -> RunSpec {
    let mut s = spec.clone();
    s.workload.rw_mix_pct_read = 0;
    s.workload.pattern = "seq".into();
    s.workload.block_size = 1024 * 1024;
    s.workload.direct_io = true;
    s.workload.duration_s = None;
    // drop_caches / sync are irrelevant for layout-only — leave at user's
    // choice. extra_flags pass through so user can force anything if they
    // really need to.
    s
}

/// Top-level entry: pick local or fan-out path.
pub fn run(
    spec: &RunSpec,
    spec_dir: &Path,
    timeout_s: Option<u64>,
    backend: &dyn Backend,
) -> Result<RunResult> {
    if is_localhost_only(&spec.clients) {
        run_locally(spec, spec_dir, timeout_s, backend)
    } else {
        // SSH fan-out uses tokio; spin a runtime for the duration of this run.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(run_fanout(spec, spec_dir, timeout_s, backend))
    }
}

pub fn run_locally(
    spec: &RunSpec,
    spec_dir: &Path,
    timeout_s: Option<u64>,
    backend: &dyn Backend,
) -> Result<RunResult> {
    let lp = local_path(spec);
    let version = backend.detect_version(&lp).with_context(|| {
        format!(
            "{} not found at {:?}; install it and ensure it's on PATH \
             (or set clients[0].elbencho_path)",
            backend.name(),
            lp
        )
    })?;

    let support = backend.supports_target(&spec.target);
    if !support.supported {
        anyhow::bail!("{}", support.reason);
    }
    if backend.name() == "elbencho" {
        if let Target::S3(_) = &spec.target {
            if !version.has("S3") {
                anyhow::bail!(
                    "elbencho was not built with S3_SUPPORT=1; \
                     rebuild with `make S3_SUPPORT=1` or use the breuner/elbencho Docker image"
                );
            }
        }
    }

    ensure_posix_dataset_dir(spec)?;

    let mut env: Vec<(String, String)> = std::env::vars().collect();
    if let Target::S3(t) = &spec.target {
        inject_s3_credentials(&mut env, &t.credentials_ref)?;
    }

    // 1. Layout phase (if needed). Writes the dataset with fixed
    //    seq/1MiB/direct=1 settings — predictable layout time, doesn't
    //    cheat the read measurement by leaving sparse holes.
    if needs_layout(spec) {
        let layout_spec = layout_spec_for(spec);
        let layout_dir = spec_dir.join("raw_layout");
        let (exec, _) = run_one_local_pass(
            &layout_spec,
            &layout_dir,
            &lp,
            timeout_s,
            backend,
            &env,
            None,
        )?;
        let layout_rc = exec.exit_code;
        if layout_rc != 0 {
            anyhow::bail!(
                "layout phase failed (exit={}); see {}/stderr.log: {}",
                layout_rc,
                layout_dir.display(),
                String::from_utf8_lossy(&exec.stderr)
                    .lines()
                    .next()
                    .unwrap_or("(no stderr)")
            );
        }
    }

    // 2. Measure phase: the user's workload.
    let raw_dir = spec_dir.join("raw");
    let (exec, primary_phase) =
        run_one_local_pass(spec, &raw_dir, &lp, timeout_s, backend, &env, None)?;

    // Gather hardware facts for the (single, local) client. Best-effort:
    // a None just means the report shows no system panel for this host.
    let mut client_systems: HashMap<String, crate::results::schema::SystemInfo> =
        HashMap::new();
    if let Some(c) = spec.clients.first() {
        if let Some(info) = crate::engine::sysinfo::gather_local_blocking(&c.host) {
            client_systems.insert(c.host.clone(), info);
        }
    }

    let (phases, artifact_refs) = backend.parse_results(&raw_dir, &exec.command_str)?;
    Ok(build_result(
        spec,
        backend,
        version.version.as_deref(),
        &version.features,
        artifact_refs,
        phases,
        exec.started,
        exec.finished,
        exec.exit_code,
        primary_phase,
        &String::from_utf8_lossy(&exec.stderr),
        &client_systems,
    ))
}

/// Outcome of one engine invocation, local or on the bastion. Owning
/// plain fields (not std::process::Output) lets the jump-host path build
/// one from an SshResult without fabricating an ExitStatus.
struct PassExec {
    exit_code: i32,
    stderr: Vec<u8>,
    command_str: String,
    started: chrono::DateTime<Utc>,
    finished: chrono::DateTime<Utc>,
}

/// One backend invocation. `hosts` is `Some(h1:p,h2:p,...)` for fan-out
/// or None for local. Writes stdout.log / stderr.log into `raw_dir` so
/// the in-TUI Report viewer and `elmaestro browse` can both inspect
/// what happened.
#[allow(clippy::too_many_arguments)]
fn run_one_local_pass(
    spec: &RunSpec,
    raw_dir: &Path,
    lp: &str,
    timeout_s: Option<u64>,
    backend: &dyn Backend,
    env: &[(String, String)],
    hosts: Option<&str>,
) -> Result<(PassExec, String)> {
    std::fs::create_dir_all(raw_dir)
        .with_context(|| format!("creating {}", raw_dir.display()))?;
    let (argv, primary_phase) = backend.build_argv(spec, raw_dir, lp, hosts)?;
    let command_str = shell_quote_argv(&argv);

    let started = Utc::now();
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.envs(env.iter().cloned());
    let output = match timeout_s {
        Some(secs) => run_with_timeout(command, Duration::from_secs(secs))?,
        None => command
            .output()
            .with_context(|| format!("running {}", argv[0]))?,
    };
    let finished = Utc::now();

    std::fs::write(raw_dir.join("stdout.log"), &output.stdout)?;
    if !output.stderr.is_empty() {
        std::fs::write(raw_dir.join("stderr.log"), &output.stderr)?;
    }
    // Record the exact command we ran. The user's #1 debug ask is
    // "what did elmaestro actually invoke?" — having this in the run
    // dir means it's one `cat` away.
    let _ = std::fs::write(raw_dir.join("command.txt"), &command_str);

    Ok((
        PassExec {
            exit_code: output.status.code().unwrap_or(-1),
            stderr: output.stderr,
            command_str,
            started,
            finished,
        },
        primary_phase,
    ))
}

async fn run_fanout(
    spec: &RunSpec,
    spec_dir: &Path,
    timeout_s: Option<u64>,
    backend: &dyn Backend,
) -> Result<RunResult> {
    let lp = local_path(spec);
    let bastion = jump_bastion(spec);

    // Engine version check runs wherever the master will run: on the
    // bastion in jump mode (the local machine needs NO engine binary
    // then), locally otherwise.
    let version = match &bastion {
        Some(b) => {
            let runner = crate::engine::ssh::SshRunner::new(b.clone());
            let r = runner
                .run(&[lp.as_str(), "--version"], Some(Duration::from_secs(15)))
                .await
                .with_context(|| format!("ssh to jump host {}", b.host))?;
            if !r.ok() {
                anyhow::bail!(
                    "{} not found at {:?} on jump host {} — with a jump host \
                     configured the bastion acts as the coordinator and needs \
                     the engine binary (exit={}, stderr: {})",
                    backend.name(),
                    lp,
                    b.host,
                    r.exit_status,
                    r.stderr.lines().next().unwrap_or("")
                );
            }
            let raw = format!("{}{}", r.stdout, r.stderr);
            crate::backends::EngineVersion {
                raw: raw.trim().to_string(),
                version: crate::engine::check::parse_version(&raw),
                features: crate::engine::check::parse_features(&raw),
            }
        }
        None => backend.detect_version(&lp).with_context(|| {
            format!(
                "local {} not found at {:?}; the coordinator machine \
                 needs the binary too (it runs the master process). \
                 If you meant to coordinate from a bastion, set a jump host.",
                backend.name(),
                lp
            )
        })?,
    };
    let support = backend.supports_target(&spec.target);
    if !support.supported {
        anyhow::bail!("{}", support.reason);
    }
    // Intentionally do NOT ensure the dataset dir on the master here.
    // In fan-out the workers do the writes; the master may not even
    // have the mount, and creating an empty dir on its local fs would
    // be a footgun (silently masks a missing mount on the workers).

    // S3 credentials: resolved locally either way, but tracked separately
    // so jump mode forwards ONLY the injected pairs to the bastion (never
    // the whole local environment).
    let mut injected_env: Vec<(String, String)> = Vec::new();
    if let Target::S3(t) = &spec.target {
        inject_s3_credentials(&mut injected_env, &t.credentials_ref)?;
    }
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.extend(injected_env.iter().cloned());

    // Bring up engine services on every worker ONCE for both phases.
    // Tearing down between layout and measure would just re-pay the
    // SSH + service-start cost. In jump mode the port probe runs on each
    // worker itself — the local machine can't reach the service ports
    // behind the bastion.
    let probe_mode = if bastion.is_some() {
        crate::engine::service::ProbeMode::OnWorker
    } else {
        crate::engine::service::ProbeMode::FromCoordinator
    };
    let (endpoints, guard) = crate::engine::service::bring_up(
        backend,
        &spec.clients,
        Duration::from_secs(15),
        probe_mode,
    )
    .await?;
    let hosts = crate::engine::service::hosts_arg(&endpoints);
    // Per-spec scratch dir on the bastion for staged inputs + engine
    // output files. Removed after the passes complete.
    let remote_base = format!("/tmp/elmaestro-{}", spec.run_id);

    // 1. Layout phase: explicit dataset write before the measured workload.
    let layout_err: Option<String> = if needs_layout(spec) {
        let layout_spec = layout_spec_for(spec);
        let layout_dir = spec_dir.join("raw_layout");
        let layout_res = match &bastion {
            Some(b) => {
                run_one_jump_pass(
                    &layout_spec,
                    &layout_dir,
                    &remote_base,
                    &lp,
                    backend,
                    &injected_env,
                    &hosts,
                    b,
                )
                .await
            }
            None => {
                run_one_fanout_pass(
                    &layout_spec,
                    &layout_dir,
                    &lp,
                    timeout_s,
                    backend,
                    &env,
                    &hosts,
                )
                .await
            }
        };
        match layout_res {
            Ok((exec, _)) => {
                let rc = exec.exit_code;
                if rc != 0 {
                    let snippet = String::from_utf8_lossy(&exec.stderr)
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .take(5)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    Some(format!(
                        "layout phase failed (exit={}, see {}/stderr.log): {}",
                        rc,
                        layout_dir.display(),
                        snippet
                    ))
                } else {
                    None
                }
            }
            Err(e) => Some(format!("{:#}", e)),
        }
    } else {
        None
    };
    if let Some(msg) = layout_err {
        cleanup_remote_base(&bastion, &remote_base).await;
        guard.shutdown().await;
        anyhow::bail!(msg);
    }

    // 2. Measure phase.
    let raw_dir = spec_dir.join("raw");
    let pass_res = match &bastion {
        Some(b) => {
            run_one_jump_pass(
                spec,
                &raw_dir,
                &remote_base,
                &lp,
                backend,
                &injected_env,
                &hosts,
                b,
            )
            .await
        }
        None => {
            run_one_fanout_pass(spec, &raw_dir, &lp, timeout_s, backend, &env, &hosts).await
        }
    };
    cleanup_remote_base(&bastion, &remote_base).await;
    guard.shutdown().await;
    let (exec, primary_phase) = pass_res?;

    // Gather hardware facts for every worker in parallel. Best-effort
    // and cached per host, so a sweep only probes each host once. A
    // failure to gather never affects the run result.
    let mut sys_tasks = Vec::with_capacity(spec.clients.len());
    for c in &spec.clients {
        let c = c.clone();
        sys_tasks.push(tokio::spawn(async move {
            (c.host.clone(), crate::engine::sysinfo::gather(&c).await)
        }));
    }
    let mut client_systems: HashMap<String, crate::results::schema::SystemInfo> =
        HashMap::new();
    for t in sys_tasks {
        if let Ok((host, Some(info))) = t.await {
            client_systems.insert(host, info);
        }
    }

    let (phases, artifact_refs) = backend.parse_results(&raw_dir, &exec.command_str)?;
    Ok(build_result(
        spec,
        backend,
        version.version.as_deref(),
        &version.features,
        artifact_refs,
        phases,
        exec.started,
        exec.finished,
        exec.exit_code,
        primary_phase,
        &String::from_utf8_lossy(&exec.stderr),
        &client_systems,
    ))
}

/// One fan-out master invocation. Runs the engine binary locally (or in
/// the coordinator container), pointed at the live services via --hosts.
/// Uses spawn_blocking so we don't stall the current-thread runtime that
/// the guard's shutdown task still lives on.
#[allow(clippy::too_many_arguments)]
async fn run_one_fanout_pass(
    spec: &RunSpec,
    raw_dir: &Path,
    lp: &str,
    timeout_s: Option<u64>,
    backend: &dyn Backend,
    env: &[(String, String)],
    hosts: &str,
) -> Result<(PassExec, String)> {
    std::fs::create_dir_all(raw_dir)
        .with_context(|| format!("creating {}", raw_dir.display()))?;
    let (argv, primary_phase) = backend.build_argv(spec, raw_dir, lp, Some(hosts))?;
    let command_str = shell_quote_argv(&argv);

    let started = Utc::now();
    let argv_owned = argv.clone();
    let env_owned: Vec<(String, String)> = env.to_vec();
    let output_res = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(&argv_owned[0]);
        command.args(&argv_owned[1..]);
        command.envs(env_owned);
        match timeout_s {
            Some(secs) => run_with_timeout(command, Duration::from_secs(secs)),
            None => command
                .output()
                .with_context(|| format!("running {}", argv_owned[0])),
        }
    })
    .await
    .map_err(|e| anyhow!("master subprocess join error: {}", e))?;
    let output = output_res?;
    let finished = Utc::now();

    std::fs::write(raw_dir.join("stdout.log"), &output.stdout)?;
    if !output.stderr.is_empty() {
        std::fs::write(raw_dir.join("stderr.log"), &output.stderr)?;
    }
    let _ = std::fs::write(raw_dir.join("command.txt"), &command_str);

    Ok((
        PassExec {
            exit_code: output.status.code().unwrap_or(-1),
            stderr: output.stderr,
            command_str,
            started,
            finished,
        },
        primary_phase,
    ))
}

/// One master pass executed ON THE BASTION (jump mode). The local machine
/// stages the engine's input files (job.fio, hosts.list, ...) at a /tmp
/// path that is IDENTICAL locally and remotely — build_argv embeds
/// absolute paths in the argv, so staging at the same path keeps them
/// valid on the bastion. Inputs go over a tar pipe, the master runs over
/// SSH (holding the session for the duration of the pass), and the
/// engine's output files are pulled back into the spec's real raw dir so
/// result parsing is identical to every other mode.
#[allow(clippy::too_many_arguments)]
async fn run_one_jump_pass(
    spec: &RunSpec,
    real_raw_dir: &Path,
    remote_base: &str,
    lp: &str,
    backend: &dyn Backend,
    injected_env: &[(String, String)],
    hosts: &str,
    bastion: &ClientHost,
) -> Result<(PassExec, String)> {
    let pass_name = real_raw_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("raw");
    let remote_raw = format!("{}/{}", remote_base, pass_name);
    let staging = std::path::PathBuf::from(&remote_raw);

    let (argv, primary_phase) = backend.build_argv(spec, &staging, lp, Some(hosts))?;
    let command_str = shell_quote_argv(&argv);
    crate::engine::ssh::push_dir(bastion, &staging, &remote_raw)
        .await
        .with_context(|| format!("staging engine inputs on jump host {}", bastion.host))?;

    // Only the explicitly injected pairs (S3 credentials) go remote —
    // never the whole local environment.
    let mut remote_argv: Vec<String> = Vec::new();
    if !injected_env.is_empty() {
        remote_argv.push("env".into());
        for (k, v) in injected_env {
            remote_argv.push(format!("{}={}", k, v));
        }
    }
    remote_argv.extend(argv.iter().cloned());
    let argv_refs: Vec<&str> = remote_argv.iter().map(|s| s.as_str()).collect();

    let runner = crate::engine::ssh::SshRunner::new(bastion.clone());
    let started = Utc::now();
    let r = runner
        .run(&argv_refs, None)
        .await
        .with_context(|| format!("running {} master on jump host {}", backend.name(), bastion.host))?;
    let finished = Utc::now();

    crate::engine::ssh::pull_dir(bastion, &remote_raw, real_raw_dir)
        .await
        .with_context(|| format!("pulling engine output from jump host {}", bastion.host))?;
    std::fs::write(real_raw_dir.join("stdout.log"), r.stdout.as_bytes())?;
    if !r.stderr.is_empty() {
        std::fs::write(real_raw_dir.join("stderr.log"), r.stderr.as_bytes())?;
    }
    let _ = std::fs::write(real_raw_dir.join("command.txt"), &command_str);
    // Remove the local staging copy; the canonical artifacts now live in
    // real_raw_dir. The bastion side is removed by cleanup_remote_base.
    let _ = std::fs::remove_dir_all(&staging);

    Ok((
        PassExec {
            exit_code: r.exit_status,
            stderr: r.stderr.into_bytes(),
            command_str,
            started,
            finished,
        },
        primary_phase,
    ))
}

/// Best-effort removal of the per-spec scratch dir on the bastion.
async fn cleanup_remote_base(bastion: &Option<ClientHost>, remote_base: &str) {
    if let Some(b) = bastion {
        let runner = crate::engine::ssh::SshRunner::new(b.clone());
        let _ = runner
            .run(&["rm", "-rf", remote_base], Some(Duration::from_secs(10)))
            .await;
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<std::process::Output> {
    // std::process doesn't have a timeout. For now: run synchronously and
    // accept that a hung subprocess hangs the program. The timeout flag is
    // honored by elbencho/fio themselves via --timelimit / --runtime.
    let _ = timeout;
    command.output().with_context(|| "running subprocess")
}

#[allow(clippy::too_many_arguments)]
fn build_result(
    spec: &RunSpec,
    backend: &dyn Backend,
    engine_version: Option<&str>,
    engine_features: &[String],
    engine_artifacts: EngineArtifactRefs,
    phases: HashMap<String, PhaseResult>,
    started_at: chrono::DateTime<Utc>,
    finished_at: chrono::DateTime<Utc>,
    exit_code: i32,
    primary_phase: String,
    stderr_tail: &str,
    client_systems: &HashMap<String, crate::results::schema::SystemInfo>,
) -> RunResult {
    let tgt_snap = match &spec.target {
        Target::Posix(t) => TargetSnapshot {
            kind: "posix".into(),
            name: t.name.clone(),
            detail: vec![
                (
                    "mount_path".into(),
                    serde_json::Value::String(t.mount_path.to_string_lossy().into_owned()),
                ),
                (
                    "dataset_subdir".into(),
                    serde_json::Value::String(t.dataset_subdir.clone()),
                ),
            ]
            .into_iter()
            .collect(),
        },
        Target::S3(t) => TargetSnapshot {
            kind: "s3".into(),
            name: t.name.clone(),
            detail: vec![
                (
                    "endpoint".into(),
                    serde_json::Value::String(t.endpoint.clone()),
                ),
                (
                    "bucket".into(),
                    serde_json::Value::String(t.bucket.clone()),
                ),
                (
                    "region".into(),
                    t.region
                        .clone()
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                ),
                (
                    "addressing".into(),
                    serde_json::Value::String(t.addressing.clone()),
                ),
            ]
            .into_iter()
            .collect(),
        },
    };

    let wl = &spec.workload;
    let workload_snap = WorkloadSnapshot {
        name: wl.name.clone(),
        block_size: wl.block_size,
        rw_mix_pct_read: wl.rw_mix_pct_read,
        threads_per_client: wl.threads_per_client,
        io_depth: wl.io_depth,
        pattern: wl.pattern.clone(),
        direct_io: wl.direct_io,
        duration_s: wl.duration_s,
        dataset_size: wl.dataset_size,
        file_size: wl.file_size,
        file_count: wl.file_count,
        total_concurrency: wl.total_concurrency(spec.clients.len()),
    };

    let clients = spec
        .clients
        .iter()
        .map(|c| ClientInfo {
            host: c.host.clone(),
            elbencho_version: engine_version.map(|s| s.to_string()),
            features: engine_features.to_vec(),
            system: client_systems.get(&c.host).cloned(),
        })
        .collect();

    let mut errors = Vec::new();
    if exit_code != 0 {
        errors.push(format!("{} exited {}", backend.name(), exit_code));
        let stderr_tail_trim = if stderr_tail.len() > 2000 {
            &stderr_tail[stderr_tail.len() - 2000..]
        } else {
            stderr_tail
        };
        if !stderr_tail_trim.is_empty() {
            errors.push(stderr_tail_trim.to_string());
        }
    }
    let duration_s = (finished_at - started_at).num_milliseconds() as f64 / 1000.0;
    let duration_s = duration_s.max(0.0);

    RunResult {
        schema_version: SCHEMA_VERSION.into(),
        run_id: spec.run_id.clone(),
        spec_hash: spec.spec_hash.clone(),
        engine: backend.name().into(),
        primary_phase,
        started_at,
        finished_at,
        duration_s,
        target: tgt_snap,
        workload: workload_snap,
        clients,
        elbencho: engine_artifacts,
        phases,
        elbencho_exit_code: exit_code,
        errors,
        notes: String::new(),
    }
}

fn shell_quote_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|s| {
            shlex::try_quote(s)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| s.clone())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn inject_s3_credentials(env: &mut Vec<(String, String)>, reference: &str) -> Result<()> {
    if let Some(name) = reference.strip_prefix("env:") {
        let value = std::env::var(name).map_err(|_| {
            anyhow!("credentials_ref points at env var {:?} but it's unset", name)
        })?;
        let already_set = env.iter().any(|(k, _)| k == "AWS_ACCESS_KEY_ID");
        if !already_set {
            if let Some((access, secret)) = value.split_once(':') {
                env.push(("AWS_ACCESS_KEY_ID".into(), access.into()));
                env.push(("AWS_SECRET_ACCESS_KEY".into(), secret.into()));
            }
        }
        return Ok(());
    }
    if let Some(path) = reference.strip_prefix("file:") {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("credentials_ref file not found: {}", path))?;
        let first = content.lines().next().unwrap_or("").trim();
        if let Some((access, secret)) = first.split_once(':') {
            env.push(("AWS_ACCESS_KEY_ID".into(), access.trim().into()));
            env.push(("AWS_SECRET_ACCESS_KEY".into(), secret.trim().into()));
        } else {
            let mut lines = content.lines();
            let access = lines.next().unwrap_or("").trim();
            let secret = lines.next().unwrap_or("").trim();
            env.push(("AWS_ACCESS_KEY_ID".into(), access.into()));
            env.push(("AWS_SECRET_ACCESS_KEY".into(), secret.into()));
        }
        return Ok(());
    }
    Err(anyhow!("unsupported credentials_ref scheme: {:?}", reference))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientHost;

    #[test]
    fn is_localhost_only_true_for_single_localhost() {
        assert!(is_localhost_only(&[ClientHost {
            host: "localhost".into(),
            ..Default::default()
        }]));
        assert!(is_localhost_only(&[ClientHost {
            host: "127.0.0.1".into(),
            ..Default::default()
        }]));
    }

    #[test]
    fn is_localhost_only_false_for_remote() {
        assert!(!is_localhost_only(&[ClientHost {
            host: "worker-01".into(),
            ..Default::default()
        }]));
    }

    #[test]
    fn is_localhost_only_false_for_multiple_clients() {
        assert!(!is_localhost_only(&[
            ClientHost {
                host: "localhost".into(),
                ..Default::default()
            },
            ClientHost {
                host: "h2".into(),
                ..Default::default()
            },
        ]));
    }

    #[test]
    fn inject_credentials_env_with_colon_split() {
        std::env::set_var("BENCH_S3_TEST_X", "AKIA:secret");
        let mut env: Vec<(String, String)> = Vec::new();
        inject_s3_credentials(&mut env, "env:BENCH_S3_TEST_X").unwrap();
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map["AWS_ACCESS_KEY_ID"], "AKIA");
        assert_eq!(map["AWS_SECRET_ACCESS_KEY"], "secret");
    }

    #[test]
    fn inject_credentials_unknown_scheme_errors() {
        let mut env: Vec<(String, String)> = Vec::new();
        assert!(inject_s3_credentials(&mut env, "inline:AKIA:secret").is_err());
    }
}
