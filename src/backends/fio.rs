//! fio backend.
//!
//! Drives [fio](https://github.com/axboe/fio). Generates a deterministic job
//! file under `raw/job.fio`, invokes fio with `--output-format=json
//! --output=raw/run.json`, and parses the JSON afterward. Handles fio's
//! client/server preamble (host-prefixed status lines before the JSON
//! document) and prefers the "All clients" aggregate when multi-client.
//!
//! S3 targets are deferred; fio's S3 engines are weaker than elbencho's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::{ClientHost, RunSpec, Target};
use crate::results::schema::{EngineArtifactRefs, LatencyBucket, PhaseResult};

use super::{Backend, EngineVersion, TargetSupport};

pub struct FioBackend;

impl FioBackend {
    pub fn new() -> Self {
        FioBackend
    }
}

impl Default for FioBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn artifact_paths(raw_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        raw_dir.join("job.fio"),
        raw_dir.join("run.json"),
        raw_dir.join("stdout.log"),
    )
}

/// Map (pattern, rw_mix_pct_read) -> fio's --rw value.
fn fio_rw(pattern: &str, rw_mix_pct_read: u8) -> &'static str {
    if pattern == "rand" {
        match rw_mix_pct_read {
            100 => "randread",
            0 => "randwrite",
            _ => "randrw",
        }
    } else {
        match rw_mix_pct_read {
            100 => "read",
            0 => "write",
            _ => "rw",
        }
    }
}

fn primary_phase_for(rw_mix_pct_read: u8) -> &'static str {
    match rw_mix_pct_read {
        100 => "read",
        0 => "write",
        _ => "mixed",
    }
}

impl Backend for FioBackend {
    fn name(&self) -> &'static str {
        "fio"
    }

    fn detect_version(&self, local_path: &str) -> Result<EngineVersion> {
        let out = Command::new(local_path)
            .arg("--version")
            .output()
            .with_context(|| format!("running {} --version", local_path))?;
        let raw = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .trim()
        .to_string();
        let version = Regex::new(r"fio[-\s]+(\d+\.\d+(?:\.\d+)?)")
            .unwrap()
            .captures(&raw)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        Ok(EngineVersion {
            raw,
            version,
            features: Vec::new(),
        })
    }

    fn build_argv(
        &self,
        spec: &RunSpec,
        raw_dir: &Path,
        local_path: &str,
        hosts: Option<&str>,
    ) -> Result<(Vec<String>, String)> {
        std::fs::create_dir_all(raw_dir)
            .with_context(|| format!("creating {}", raw_dir.display()))?;
        let (job_file, json_out, _stdout) = artifact_paths(raw_dir);

        let wl = &spec.workload;
        let posix = match &spec.target {
            Target::Posix(t) => t,
            Target::S3(_) => anyhow::bail!("fio backend only supports POSIX targets"),
        };
        let dataset_dir = posix.mount_path.join(&posix.dataset_subdir);
        let rw = fio_rw(&wl.pattern, wl.rw_mix_pct_read);
        let primary_phase = primary_phase_for(wl.rw_mix_pct_read);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("[{}]", wl.name));
        lines.push("ioengine=psync".into());
        lines.push(format!("directory={}", dataset_dir.display()));
        lines.push(format!("rw={}", rw));
        lines.push(format!("bs={}", wl.block_size));
        lines.push(format!("iodepth={}", wl.io_depth));
        lines.push(format!("numjobs={}", wl.threads_per_client));
        if let Some(s) = wl.file_size {
            lines.push(format!("size={}", s));
        }
        if let Some(n) = wl.file_count {
            if n > 1 {
                lines.push(format!("nrfiles={}", n));
            }
        }
        if wl.direct_io {
            lines.push("direct=1".into());
        }
        if wl.sync_after_write {
            lines.push("end_fsync=1".into());
        }
        if let Some(d) = wl.duration_s {
            lines.push(format!("runtime={}", d));
            lines.push("time_based=1".into());
        }
        if wl.rw_mix_pct_read > 0 && wl.rw_mix_pct_read < 100 {
            lines.push(format!("rwmixread={}", wl.rw_mix_pct_read));
        }
        lines.push("group_reporting=1".into());
        for flag in &wl.extra_flags {
            lines.push(flag.clone());
        }
        std::fs::write(&job_file, format!("{}\n", lines.join("\n")))
            .with_context(|| format!("writing {}", job_file.display()))?;

        let mut argv: Vec<String> = vec![local_path.into()];
        argv.push("--output-format=json".into());
        argv.push(format!("--output={}", json_out.display()));
        // Periodic full status dump appended to the output file. Feeds the
        // Run screen's live progress display; the final parse always takes
        // the LAST complete JSON object (see load_fio_json).
        argv.push("--status-interval=1".into());
        if let Some(h) = hosts {
            // fio's --client takes "host,port" (comma); our hosts string uses
            // "host:port" (colon) from the elbencho convention. Translate.
            for hp in h.split(',') {
                let (host, port) = hp.split_once(':').unwrap_or((hp, ""));
                if !port.is_empty() {
                    argv.push(format!("--client={},{}", host, port));
                } else {
                    argv.push(format!("--client={}", host));
                }
            }
        }
        argv.push(job_file.to_string_lossy().into_owned());

        Ok((argv, primary_phase.into()))
    }

    fn parse_results(
        &self,
        raw_dir: &Path,
        command: &str,
    ) -> Result<(HashMap<String, PhaseResult>, EngineArtifactRefs)> {
        let (_, json_path, stdout_path) = artifact_paths(raw_dir);
        let mut phases: HashMap<String, PhaseResult> = HashMap::new();
        if json_path.is_file() {
            if let Some(value) = load_fio_json(&json_path) {
                phases = phases_from_fio_json(&value);
            }
        }
        let refs = EngineArtifactRefs {
            command: command.into(),
            stdout_path: stdout_path.to_string_lossy().into_owned(),
            csv_path: None,
            jsonfile_path: Some(json_path.to_string_lossy().into_owned()),
            livecsv_path: None,
        };
        Ok((phases, refs))
    }

    fn supports_target(&self, target: &Target) -> TargetSupport {
        match target {
            Target::Posix(_) => TargetSupport::yes(),
            Target::S3(_) => TargetSupport::no(
                "fio's S3 ioengines are weaker than elbencho's; \
                 v1.0 keeps S3 on the elbencho backend. \
                 Set `engine: elbencho` for S3 targets.",
            ),
        }
    }

    fn service_command(&self, client: &ClientHost) -> Vec<String> {
        // fio server endpoint syntax is `<type>:<host>,<port>`; a leading
        // comma means "all interfaces". `--server=,8765` listens on every
        // interface, port 8765. (The pre-v1.9.1 `,N:port` form made fio
        // exit instantly with "bad server port 0".)
        vec![
            client.elbencho_path.clone(),
            format!("--server=,{}", client.service_port),
        ]
    }
}

// ---------------------------------------------------------------------------
// JSON loader. The output file may contain, in any combination: a
// host-prefixed preamble (client mode), MULTIPLE concatenated JSON dumps
// (--status-interval appends a cumulative dump every second), and a
// truncated trailing object (read mid-write). We always want the LAST
// complete top-level object — during the run that's the freshest live
// dump, after the run it's fio's final summary.
// ---------------------------------------------------------------------------

fn load_fio_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    last_complete_json(&text)
}

/// Scan for top-level `{...}` spans (brace-depth counting, string- and
/// escape-aware) and parse the last complete one. Text outside objects
/// (preambles, log lines between dumps) is ignored.
fn last_complete_json(text: &str) -> Option<serde_json::Value> {
    let bytes = text.as_bytes();
    let mut last_span: Option<(usize, usize)> = None;
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' if depth > 0 => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        last_span = Some((start, i + 1));
                    }
                }
            }
            _ => {}
        }
    }
    let (s, e) = last_span?;
    serde_json::from_str::<serde_json::Value>(&text[s..e]).ok()
}

/// Best-effort live stats for a run in flight: parse the freshest
/// complete dump in raw/run.json and sum throughput/IOPS across the
/// phases present. Returns None until fio's first --status-interval
/// dump lands.
pub fn live_metrics(raw_dir: &Path) -> Option<crate::backends::LiveStats> {
    let (_, json_path, _) = artifact_paths(raw_dir);
    let value = load_fio_json(&json_path)?;
    let phases = phases_from_fio_json(&value);
    if phases.is_empty() {
        return None;
    }
    let mut tput = 0.0f64;
    let mut iops = 0.0f64;
    let mut have_tput = false;
    let mut have_iops = false;
    for p in phases.values() {
        if let Some(t) = p.throughput_mib_s_last.or(p.throughput_mib_s_first) {
            tput += t;
            have_tput = true;
        }
        if let Some(i) = p.iops_last.or(p.iops_first) {
            iops += i;
            have_iops = true;
        }
    }
    Some(crate::backends::LiveStats {
        throughput_mib_s: have_tput.then_some(tput),
        iops: have_iops.then_some(iops),
    })
}

fn phases_from_fio_json(data: &serde_json::Value) -> HashMap<String, PhaseResult> {
    let mut out: HashMap<String, PhaseResult> = HashMap::new();

    // Prefer the "All clients" aggregate from client_stats when present.
    let source: Option<&serde_json::Value> = if let Some(cs) = data.get("client_stats").and_then(|v| v.as_array()) {
        // Look for hostname == "All clients" from the end.
        let agg = cs.iter().rev().find(|c| {
            c.get("hostname")
                .and_then(|h| h.as_str())
                .map_or(false, |h| h == "All clients")
        });
        agg.or_else(|| cs.first())
    } else if let Some(jobs) = data.get("jobs").and_then(|v| v.as_array()) {
        jobs.first()
    } else {
        None
    };

    let Some(source) = source else {
        return out;
    };

    for op in ["read", "write", "mixed"] {
        let section = match source.get(op) {
            Some(s) if s.is_object() => s,
            _ => continue,
        };
        let io_bytes = section.get("io_bytes").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let iops = section.get("iops").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if io_bytes == 0.0 && iops == 0.0 {
            // fio writes empty sections for the unused side of pure read /
            // pure write; skip those.
            continue;
        }
        out.insert(op.into(), phase_from_fio_section(op, section));
    }
    out
}

fn phase_from_fio_section(op: &str, sec: &serde_json::Value) -> PhaseResult {
    let bw_kib_s = sec.get("bw").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let iops = sec.get("iops").and_then(|v| v.as_f64());
    let iops_mean = sec.get("iops_mean").and_then(|v| v.as_f64());
    let iops_final = iops.or(iops_mean);
    let tput_mib_s = if bw_kib_s > 0.0 {
        Some(bw_kib_s / 1024.0)
    } else {
        None
    };

    let clat_ns = sec.get("clat_ns");
    let lat_min = clat_ns.and_then(|n| n.get("min")).and_then(|v| v.as_f64());
    let lat_max = clat_ns.and_then(|n| n.get("max")).and_then(|v| v.as_f64());
    let lat_mean = clat_ns.and_then(|n| n.get("mean")).and_then(|v| v.as_f64());
    let to_us = |ns: Option<f64>| ns.map(|n| n / 1000.0);

    let mut pct_us: HashMap<String, f64> = HashMap::new();
    if let Some(pct_section) = clat_ns
        .and_then(|n| n.get("percentile"))
        .and_then(|v| v.as_object())
    {
        for (k, v) in pct_section {
            let Some(val_ns) = v.as_f64() else {
                continue;
            };
            // Normalize "99.000000" -> "p99", "99.900000" -> "p99.9"
            let Ok(pct) = k.parse::<f64>() else { continue };
            let label = if (pct - pct.floor()).abs() < f64::EPSILON {
                format!("p{}", pct as i64)
            } else {
                // Trim trailing zeros after the decimal.
                let mut s = format!("{}", pct);
                while s.ends_with('0') {
                    s.pop();
                }
                if s.ends_with('.') {
                    s.pop();
                }
                format!("p{}", s)
            };
            pct_us.insert(label, val_ns / 1000.0);
        }
    }

    let bytes_total = sec.get("io_bytes").and_then(|v| v.as_f64());
    let mib_total = bytes_total.map(|b| b / (1024.0 * 1024.0));

    // Capture scalar raw fields for forensic value.
    let mut raw: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(map) = sec.as_object() {
        for (k, v) in map {
            if !matches!(v, serde_json::Value::Object(_) | serde_json::Value::Array(_)) {
                raw.insert(k.clone(), v.clone());
            }
        }
    }

    PhaseResult {
        operation: op.into(),
        throughput_mib_s_first: tput_mib_s,
        throughput_mib_s_last: tput_mib_s,
        iops_first: iops_final,
        iops_last: iops_final,
        ops_per_s_first: None,
        ops_per_s_last: None,
        entries: None,
        mib_total,
        cpu_pct: None,
        errors: 0,
        io_lat_us: LatencyBucket {
            min: to_us(lat_min),
            avg: to_us(lat_mean),
            max: to_us(lat_max),
        },
        ent_lat_us: LatencyBucket::default(),
        latency_percentiles_us: pct_us,
        raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PosixTarget, S3Target, Workload};
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn last_complete_json_takes_freshest_dump() {
        // --status-interval appends one cumulative dump per second; the
        // parser must take the LAST complete object and ignore both the
        // client-mode preamble and a truncated trailing write.
        let text = r#"hostname: w1, be=1
{"jobs": [{"read": {"iops": 100.0}}]}
{"jobs": [{"read": {"iops": 250.0}}]}
{"jobs": [{"read": {"io"#;
        let v = last_complete_json(text).expect("should find a complete object");
        assert_eq!(
            v["jobs"][0]["read"]["iops"].as_f64(),
            Some(250.0)
        );
    }

    #[test]
    fn last_complete_json_handles_braces_inside_strings() {
        let text = r#"{"a": "literal } brace", "b": 1}
{"a": "x", "b": 2}"#;
        let v = last_complete_json(text).expect("complete object");
        assert_eq!(v["b"].as_i64(), Some(2));
    }

    #[test]
    fn last_complete_json_none_for_garbage() {
        assert!(last_complete_json("no json here").is_none());
        assert!(last_complete_json("{\"truncated\": ").is_none());
    }

    fn make_spec(target: Target, wl: Workload) -> RunSpec {
        RunSpec {
            run_id: "01".into(),
            spec_hash: "sha256:x".into(),
            target,
            workload: wl,
            clients: vec![ClientHost::default()],
        }
    }

    fn base_workload() -> Workload {
        Workload {
            name: "w".into(),
            pattern: "seq".into(),
            rw_mix_pct_read: 100,
            block_size: 1048576,
            threads_per_client: 4,
            io_depth: 4,
            direct_io: true,
            sync_after_write: false,
            drop_caches_before: false,
            duration_s: None,
            dataset_size: None,
            file_size: Some(4096),
            file_count: Some(1),
            s3_multipart_size: None,
            s3_object_prefix: None,
            extra_flags: vec![],
        }
    }

    #[test]
    fn rw_mapping() {
        assert_eq!(fio_rw("seq", 100), "read");
        assert_eq!(fio_rw("seq", 0), "write");
        assert_eq!(fio_rw("seq", 70), "rw");
        assert_eq!(fio_rw("rand", 100), "randread");
        assert_eq!(fio_rw("rand", 0), "randwrite");
        assert_eq!(fio_rw("rand", 30), "randrw");
    }

    #[test]
    fn build_argv_writes_job_file_with_required_keys() {
        let tmp = TempDir::new().unwrap();
        let spec = make_spec(
            Target::Posix(PosixTarget {
                name: "t".into(),
                mount_path: "/mnt".into(),
                dataset_subdir: "bench".into(),
                cleanup: false,
            }),
            base_workload(),
        );
        let (argv, phase) =
            FioBackend::new().build_argv(&spec, tmp.path(), "/usr/bin/fio", None).unwrap();
        let job = std::fs::read_to_string(tmp.path().join("job.fio")).unwrap();
        assert!(job.contains("[w]"));
        assert!(job.contains("directory=/mnt/bench"));
        assert!(job.contains("rw=read"));
        assert!(job.contains("bs=1048576"));
        assert!(job.contains("numjobs=4"));
        assert!(job.contains("direct=1"));
        assert!(job.contains("group_reporting=1"));
        assert_eq!(phase, "read");
        assert!(argv.iter().any(|a| a == "--output-format=json"));
        assert!(argv.iter().any(|a| a.starts_with("--output=")));
    }

    #[test]
    fn build_argv_translates_host_port_to_comma_format() {
        let tmp = TempDir::new().unwrap();
        let spec = make_spec(
            Target::Posix(PosixTarget {
                name: "t".into(),
                mount_path: "/mnt".into(),
                dataset_subdir: "bench".into(),
                cleanup: false,
            }),
            base_workload(),
        );
        let (argv, _) = FioBackend::new()
            .build_argv(&spec, tmp.path(), "/usr/bin/fio", Some("h1:8765,h2:8765"))
            .unwrap();
        assert!(argv.contains(&"--client=h1,8765".to_string()));
        assert!(argv.contains(&"--client=h2,8765".to_string()));
        // Don't leak the elbencho format.
        assert!(!argv.iter().any(|a| a == "--client=h1:8765"));
    }

    #[test]
    fn build_argv_rejects_s3() {
        let tmp = TempDir::new().unwrap();
        let spec = make_spec(
            Target::S3(S3Target {
                name: "s3".into(),
                endpoint: "https://s3".into(),
                bucket: "b".into(),
                region: None,
                credentials_ref: "env:X".into(),
                addressing: "path".into(),
            }),
            base_workload(),
        );
        assert!(FioBackend::new()
            .build_argv(&spec, tmp.path(), "/usr/bin/fio", None)
            .is_err());
    }

    fn sample_json_pure_read() -> serde_json::Value {
        json!({
            "fio version": "fio-3.36",
            "jobs": [{
                "jobname": "bench",
                "read": {
                    "io_bytes": 1_073_741_824,
                    "bw": 9_216_000,
                    "iops": 9000,
                    "clat_ns": {
                        "min": 12_000,
                        "max": 7_800_000,
                        "mean": 612_000,
                        "percentile": {
                            "50.000000": 480_000,
                            "99.000000": 1_500_000,
                            "99.900000": 3_100_000
                        }
                    }
                },
                "write": { "io_bytes": 0, "bw": 0, "iops": 0, "clat_ns": {} }
            }]
        })
    }

    #[test]
    fn parse_results_extracts_read_phase() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("run.json"),
            serde_json::to_string(&sample_json_pure_read()).unwrap(),
        )
        .unwrap();
        let (phases, _refs) = FioBackend::new()
            .parse_results(tmp.path(), "fio ...")
            .unwrap();
        let read = phases.get("read").expect("read phase");
        // bw=9_216_000 KiB/s -> 9000 MiB/s
        assert!((read.throughput_mib_s_last.unwrap() - 9000.0).abs() < 0.01);
        assert_eq!(read.iops_last, Some(9000.0));
        // clat mean 612000 ns -> 612 us
        assert_eq!(read.io_lat_us.avg, Some(612.0));
        assert_eq!(read.io_lat_us.min, Some(12.0));
        assert_eq!(read.io_lat_us.max, Some(7800.0));
        // Percentiles normalized.
        assert!((read.latency_percentiles_us["p50"] - 480.0).abs() < 0.01);
        assert!((read.latency_percentiles_us["p99"] - 1500.0).abs() < 0.01);
        assert!((read.latency_percentiles_us["p99.9"] - 3100.0).abs() < 0.01);
    }

    #[test]
    fn parse_results_skips_empty_write_phase() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("run.json"),
            serde_json::to_string(&sample_json_pure_read()).unwrap(),
        )
        .unwrap();
        let (phases, _) = FioBackend::new()
            .parse_results(tmp.path(), "fio ...")
            .unwrap();
        assert!(!phases.contains_key("write"));
    }

    #[test]
    fn parse_results_strips_client_server_preamble() {
        let tmp = TempDir::new().unwrap();
        let body = format!(
            "<worker01> seq-read-base: (g=0): rw=read\n\
             <worker01> Starting 8 processes\n\
             <worker01> seq-read-base:\n{}",
            serde_json::to_string(&sample_json_pure_read()).unwrap()
        );
        std::fs::write(tmp.path().join("run.json"), body).unwrap();
        let (phases, _) = FioBackend::new()
            .parse_results(tmp.path(), "fio ...")
            .unwrap();
        assert!(phases.contains_key("read"));
    }

    #[test]
    fn parse_results_picks_all_clients_aggregate() {
        let tmp = TempDir::new().unwrap();
        let data = json!({
            "fio version": "fio-3.36",
            "client_stats": [
                {"hostname": "h1", "read": {"io_bytes": 5e8, "bw": 4_000_000, "iops": 4000, "clat_ns": {"min": 100, "max": 200, "mean": 150}}},
                {"hostname": "h2", "read": {"io_bytes": 5e8, "bw": 5_000_000, "iops": 5000, "clat_ns": {"min": 100, "max": 200, "mean": 150}}},
                {"hostname": "All clients", "read": {"io_bytes": 1e9, "bw": 9_000_000, "iops": 9000, "clat_ns": {"min": 100, "max": 200, "mean": 150}}}
            ]
        });
        std::fs::write(
            tmp.path().join("run.json"),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
        let (phases, _) = FioBackend::new()
            .parse_results(tmp.path(), "fio ...")
            .unwrap();
        assert_eq!(phases["read"].iops_last, Some(9000.0));
    }

    #[test]
    fn does_not_support_s3() {
        let backend = FioBackend::new();
        let s3 = Target::S3(S3Target {
            name: "s3".into(),
            endpoint: "https://s3".into(),
            bucket: "b".into(),
            region: None,
            credentials_ref: "env:X".into(),
            addressing: "path".into(),
        });
        let sup = backend.supports_target(&s3);
        assert!(!sup.supported);
        assert!(sup.reason.contains("S3"));
    }

    #[test]
    fn service_command_uses_fio_server_bind() {
        let backend = FioBackend::new();
        let client = ClientHost {
            elbencho_path: "/usr/local/bin/fio".into(),
            service_port: 8765,
            ..Default::default()
        };
        assert_eq!(
            backend.service_command(&client),
            vec!["/usr/local/bin/fio", "--server=,8765"]
        );
    }
}
