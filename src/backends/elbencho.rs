//! elbencho backend.
//!
//! Drives the [elbencho](https://github.com/breuner/elbencho) workload
//! generator. CSV parser is the primary source of metrics; JSON output is
//! consulted for latency percentiles (best-effort, version-dependent).

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::{RunSpec, Target};
use crate::results::schema::{EngineArtifactRefs, LatencyBucket, PhaseResult};

use super::{Backend, EngineVersion, TargetSupport};

pub struct ElbenchoBackend;

impl ElbenchoBackend {
    pub fn new() -> Self {
        ElbenchoBackend
    }
}

impl Default for ElbenchoBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Filenames the backend writes/reads within `raw_dir`.
fn artifact_paths(raw_dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        raw_dir.join("run.csv"),
        raw_dir.join("run.json"),
        raw_dir.join("run.txt"),
        raw_dir.join("stdout.log"),
    )
}

impl Backend for ElbenchoBackend {
    fn name(&self) -> &'static str {
        "elbencho"
    }

    fn detect_version(&self, local_path: &str) -> Result<EngineVersion> {
        let out = Command::new(local_path)
            .arg("--version")
            .output()
            .with_context(|| format!("running {} --version", local_path))?;
        let raw_text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let version = Regex::new(r"(?i)version[:\s]+v?(\d+\.\d+(?:[.\-]\d+)?[^\s]*)")
            .unwrap()
            .captures(&raw_text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        // Real elbencho prints features lowercase ("s3 syncfs syscallh"),
        // so match case-insensitively. Canonical capitalization is emitted
        // so EngineVersion::has("S3") works against any build output.
        let mut features = Vec::new();
        for feat in ["S3", "CUDA", "CUFILE"] {
            let re = Regex::new(&format!(r"(?i)\b{}\b", feat)).unwrap();
            if re.is_match(&raw_text) {
                features.push(feat.into());
            }
        }
        Ok(EngineVersion {
            raw: raw_text.trim().to_string(),
            version,
            features,
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
        let (csv, jsonfile, resfile, _stdout) = artifact_paths(raw_dir);

        let wl = &spec.workload;
        let mut argv: Vec<String> = vec![local_path.into()];

        // Always-on structured output.
        argv.extend(["--csvfile", csv.to_string_lossy().as_ref()].map(String::from));
        argv.extend(["--jsonfile", jsonfile.to_string_lossy().as_ref()].map(String::from));
        argv.extend(["--resfile", resfile.to_string_lossy().as_ref()].map(String::from));
        argv.extend(["--latpercent", "--latpercent9s", "3"].map(String::from));

        // Multi-client fan-out.
        if let Some(h) = hosts {
            argv.extend(["--hosts", h].map(String::from));
        }

        // Workload basics.
        argv.extend(["-b", &wl.block_size.to_string()].map(String::from));
        argv.extend(["-t", &wl.threads_per_client.to_string()].map(String::from));
        if wl.io_depth > 1 {
            argv.extend(["--iodepth", &wl.io_depth.to_string()].map(String::from));
        }
        if wl.pattern == "rand" {
            argv.push("--rand".into());
        }

        // POSIX-only knobs.
        let is_posix = matches!(spec.target, Target::Posix(_));
        if is_posix {
            if wl.direct_io {
                argv.push("--direct".into());
            }
            if wl.drop_caches_before {
                argv.push("--dropcache".into());
            }
            if wl.sync_after_write {
                argv.push("--sync".into());
            }
        }

        // Dataset sizing.
        if let Some(s) = wl.file_size {
            argv.extend(["-s", &s.to_string()].map(String::from));
        }
        if let Some(n) = wl.file_count {
            argv.extend(["-N", &n.to_string()].map(String::from));
        }
        if let Some(d) = wl.duration_s {
            argv.extend(["--timelimit", &d.to_string()].map(String::from));
        }

        // rw mix -> phase flags.
        let primary_phase = match wl.rw_mix_pct_read {
            100 => {
                argv.extend(["-w", "-r"].map(String::from));
                "read"
            }
            0 => {
                argv.push("-w".into());
                "write"
            }
            _ => {
                argv.extend(
                    ["-w", "--rwmixpct", &wl.rw_mix_pct_read.to_string()].map(String::from),
                );
                "mixed"
            }
        };

        // Target args.
        match &spec.target {
            Target::Posix(t) => {
                argv.push("--mkdirs".into());
                let dataset = t.mount_path.join(&t.dataset_subdir);
                argv.push(dataset.to_string_lossy().into_owned());
            }
            Target::S3(t) => {
                argv.extend(["--s3endpoints", &t.endpoint].map(String::from));
                if let Some(r) = &t.region {
                    argv.extend(["--s3region", r].map(String::from));
                }
                if t.addressing == "virtual" {
                    argv.push("--s3virtaddr".into());
                }
                if let Some(m) = wl.s3_multipart_size {
                    argv.extend(["--s3multipartsize", &m.to_string()].map(String::from));
                }
                if let Some(p) = &wl.s3_object_prefix {
                    if !p.is_empty() {
                        argv.extend(["--s3objectprefix", p].map(String::from));
                    }
                }
                argv.push(t.bucket.clone());
            }
        }

        // User escape hatch (must come last).
        argv.extend(wl.extra_flags.iter().cloned());

        Ok((argv, primary_phase.into()))
    }

    fn parse_results(
        &self,
        raw_dir: &Path,
        command: &str,
    ) -> Result<(HashMap<String, PhaseResult>, EngineArtifactRefs)> {
        let (csv_path, jsonfile_path, _resfile, stdout_path) = artifact_paths(raw_dir);

        let mut phases: HashMap<String, PhaseResult> = HashMap::new();
        if csv_path.is_file() {
            for row in parse_csv(&csv_path)? {
                let phase = phase_from_row(&row);
                if NON_IO_OPERATIONS.contains(&phase.operation.as_str()) {
                    continue;
                }
                phases.entry(phase.operation.clone()).or_insert(phase);
            }
        }
        // Best-effort: merge percentiles from --jsonfile if present.
        if jsonfile_path.is_file() {
            if let Ok(blob) = std::fs::read_to_string(&jsonfile_path) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&blob) {
                    for (label, pcts) in extract_percentiles(&json) {
                        for (phase_name, phase) in phases.iter_mut() {
                            let label_l = label.to_ascii_lowercase();
                            if label_l.contains(phase_name) || phase_name.contains(&label_l) {
                                for (k, v) in &pcts {
                                    phase.latency_percentiles_us.insert(k.clone(), *v);
                                }
                            }
                        }
                    }
                }
            }
        }

        let livecsv = raw_dir.join("live.csv");
        let refs = EngineArtifactRefs {
            command: command.into(),
            stdout_path: stdout_path.to_string_lossy().into_owned(),
            csv_path: Some(csv_path.to_string_lossy().into_owned()),
            jsonfile_path: Some(jsonfile_path.to_string_lossy().into_owned()),
            livecsv_path: if livecsv.is_file() {
                Some(livecsv.to_string_lossy().into_owned())
            } else {
                None
            },
        };
        Ok((phases, refs))
    }

    fn supports_target(&self, _target: &Target) -> TargetSupport {
        TargetSupport::yes()
    }

    fn service_command(&self, client: &crate::config::ClientHost) -> Vec<String> {
        vec![
            client.elbencho_path.clone(),
            "--service".into(),
            "--port".into(),
            client.service_port.to_string(),
        ]
    }
}

// ---------------------------------------------------------------------------
// CSV parser
// ---------------------------------------------------------------------------

/// Non-IO operations that show up in elbencho CSV but shouldn't be reported
/// as IO phases (the harness's v0.7 MKDIRS regression fix).
const NON_IO_OPERATIONS: &[&str] =
    &["mkdirs", "rmdirs", "sync", "drop_caches", "cleanup"];

#[derive(Debug, Default)]
struct CsvRow {
    operation_raw: String,
    metrics: HashMap<String, Option<f64>>,
    raw: HashMap<String, String>,
}

fn parse_csv(path: &Path) -> Result<Vec<CsvRow>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(file);
    let headers: Vec<String> = reader
        .headers()
        .with_context(|| "reading CSV header")?
        .iter()
        .map(String::from)
        .collect();

    let cols = MetricColumns::resolve(&headers);
    let mut rows = Vec::new();
    for rec in reader.records() {
        let rec = match rec {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut raw = HashMap::new();
        for (i, h) in headers.iter().enumerate() {
            if let Some(v) = rec.get(i) {
                raw.insert(h.clone(), v.to_string());
            }
        }
        let op = cols
            .operation_col
            .as_deref()
            .and_then(|c| raw.get(c).cloned())
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut metrics: HashMap<String, Option<f64>> = HashMap::new();
        for (canonical, src) in cols.metric_cols.iter() {
            metrics.insert(canonical.clone(), src.as_ref().and_then(|c| {
                raw.get(c).and_then(|s| coerce_number(s))
            }));
        }
        rows.push(CsvRow {
            operation_raw: op,
            metrics,
            raw,
        });
    }
    Ok(rows)
}

#[derive(Debug, Default)]
struct MetricColumns {
    operation_col: Option<String>,
    metric_cols: Vec<(String, Option<String>)>,
}

impl MetricColumns {
    fn resolve(headers: &[String]) -> Self {
        let find = |needles: &[&str]| -> Option<String> {
            headers
                .iter()
                .find(|h| {
                    let hl = h.to_ascii_lowercase();
                    needles.iter().all(|n| hl.contains(n))
                })
                .cloned()
        };
        let operation_col = find(&["operation"]).or_else(|| find(&["op"]));
        let pairs: Vec<(&str, &[&str])> = vec![
            ("iops_first", &["iops", "first"]),
            ("iops_last", &["iops", "last"]),
            ("mibps_first", &["mib/s", "first"]),
            ("mibps_last", &["mib/s", "last"]),
            ("entries_per_s_first", &["entries/s", "first"]),
            ("entries_per_s_last", &["entries/s", "last"]),
            ("mib_total_first", &["mib", "first"]),
            ("mib_total_last", &["mib", "last"]),
            ("entries_first", &["entries", "first"]),
            ("entries_last", &["entries", "last"]),
            ("time_ms_first", &["time ms", "first"]),
            ("time_ms_last", &["time ms", "last"]),
            ("cpu_pct_first", &["cpu", "first"]),
            ("cpu_pct_last", &["cpu", "last"]),
            ("io_lat_us_min", &["io lat us", "min"]),
            ("io_lat_us_avg", &["io lat us", "avg"]),
            ("io_lat_us_max", &["io lat us", "max"]),
            ("ent_lat_us_min", &["ent lat us", "min"]),
            ("ent_lat_us_avg", &["ent lat us", "avg"]),
            ("ent_lat_us_max", &["ent lat us", "max"]),
        ];
        let metric_cols = pairs
            .into_iter()
            .map(|(canon, needles)| (canon.to_string(), find(needles)))
            .collect();
        MetricColumns {
            operation_col,
            metric_cols,
        }
    }
}

fn coerce_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty()
        || s.eq_ignore_ascii_case("n/a")
        || s.eq_ignore_ascii_case("na")
        || s == "-"
    {
        return None;
    }
    s.parse::<f64>().ok()
}

fn classify_phase(operation: &str) -> String {
    let op = operation.to_ascii_lowercase();
    let contains = |needle: &str| op.contains(needle);
    if contains("rwmix") || (contains("mix") && !contains("mixed")) {
        return "mixed".into();
    }
    if contains("read") {
        return "read".into();
    }
    if contains("write") {
        return "write".into();
    }
    if contains("mkdir") {
        return "mkdirs".into();
    }
    if contains("rmdir") {
        return "rmdirs".into();
    }
    if contains("drop") && contains("cache") {
        return "drop_caches".into();
    }
    if contains("cleanup") {
        return "cleanup".into();
    }
    if contains("sync") {
        return "sync".into();
    }
    if contains("stat") {
        return "stat".into();
    }
    if contains("del") {
        return "delete".into();
    }
    if operation.is_empty() {
        "unknown".into()
    } else {
        operation.into()
    }
}

fn phase_from_row(row: &CsvRow) -> PhaseResult {
    let m = &row.metrics;
    let get = |k: &str| -> Option<f64> { m.get(k).copied().flatten() };
    let raw_json: HashMap<String, serde_json::Value> = row
        .raw
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    PhaseResult {
        operation: classify_phase(&row.operation_raw),
        throughput_mib_s_first: get("mibps_first"),
        throughput_mib_s_last: get("mibps_last"),
        iops_first: get("iops_first"),
        iops_last: get("iops_last"),
        ops_per_s_first: None,
        ops_per_s_last: None,
        entries: get("entries_last").or(get("entries_first")),
        mib_total: get("mib_total_last").or(get("mib_total_first")),
        cpu_pct: get("cpu_pct_last").or(get("cpu_pct_first")),
        errors: 0,
        io_lat_us: LatencyBucket {
            min: get("io_lat_us_min"),
            avg: get("io_lat_us_avg"),
            max: get("io_lat_us_max"),
        },
        ent_lat_us: LatencyBucket {
            min: get("ent_lat_us_min"),
            avg: get("ent_lat_us_avg"),
            max: get("ent_lat_us_max"),
        },
        latency_percentiles_us: HashMap::new(),
        raw: raw_json,
    }
}

// ---------------------------------------------------------------------------
// JSON percentile extraction (best-effort across elbencho versions)
// ---------------------------------------------------------------------------

fn extract_percentiles(
    json: &serde_json::Value,
) -> HashMap<String, HashMap<String, f64>> {
    let mut result: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let pct_re = Regex::new(r"(?i)^p\d{1,3}(\.\d+)?$|^percentile_?\d").unwrap();
    walk(json, "default", &mut result, &pct_re);
    result
}

fn walk(
    node: &serde_json::Value,
    parent_label: &str,
    out: &mut HashMap<String, HashMap<String, f64>>,
    pct_re: &Regex,
) {
    match node {
        serde_json::Value::Object(map) => {
            let mut here: HashMap<String, f64> = HashMap::new();
            for (k, v) in map {
                if let Some(n) = v.as_f64() {
                    if pct_re.is_match(k) {
                        here.insert(k.to_ascii_lowercase(), n);
                    }
                }
            }
            if !here.is_empty() {
                let entry = out.entry(parent_label.to_string()).or_default();
                entry.extend(here);
            }
            for (k, v) in map {
                let label = if matches!(v, serde_json::Value::Object(_) | serde_json::Value::Array(_)) {
                    k.as_str()
                } else {
                    parent_label
                };
                walk(v, label, out, pct_re);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, parent_label, out, pct_re);
            }
        }
        _ => {}
    }
}

// Suppress unused-import lint while io::BufRead is used by tests.
#[allow(dead_code)]
fn _hint(_: &dyn BufRead) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClientHost, PosixTarget, S3Target, Workload};
    use tempfile::TempDir;

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
            threads_per_client: 8,
            io_depth: 4,
            direct_io: true,
            sync_after_write: false,
            drop_caches_before: false,
            duration_s: None,
            dataset_size: None,
            file_size: Some(1048576),
            file_count: Some(4),
            s3_multipart_size: None,
            s3_object_prefix: None,
            extra_flags: vec![],
        }
    }

    #[test]
    fn build_argv_pure_read_includes_both_phases() {
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
            ElbenchoBackend::new().build_argv(&spec, tmp.path(), "elbencho", None).unwrap();
        assert_eq!(phase, "read");
        assert!(argv.contains(&"-w".into()));
        assert!(argv.contains(&"-r".into()));
        assert!(argv.contains(&"--direct".into()));
        assert!(argv.contains(&"--iodepth".into()));
    }

    #[test]
    fn build_argv_includes_hosts_flag() {
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
        let (argv, _) = ElbenchoBackend::new()
            .build_argv(&spec, tmp.path(), "elbencho", Some("h1:1611,h2:1611"))
            .unwrap();
        assert!(argv.iter().any(|a| a == "--hosts"));
        assert!(argv.iter().any(|a| a == "h1:1611,h2:1611"));
    }

    #[test]
    fn build_argv_s3_passes_multipart_and_prefix() {
        let tmp = TempDir::new().unwrap();
        let mut wl = base_workload();
        wl.s3_multipart_size = Some(8 * 1024 * 1024);
        wl.s3_object_prefix = Some("bench/".into());
        let spec = make_spec(
            Target::S3(S3Target {
                name: "s3".into(),
                endpoint: "https://s3".into(),
                bucket: "b".into(),
                region: None,
                credentials_ref: "env:X".into(),
                addressing: "path".into(),
            }),
            wl,
        );
        let (argv, _) = ElbenchoBackend::new()
            .build_argv(&spec, tmp.path(), "elbencho", None)
            .unwrap();
        assert!(argv.iter().any(|a| a == "--s3multipartsize"));
        assert!(argv.iter().any(|a| a == "--s3objectprefix"));
        assert!(argv.iter().any(|a| a == "bench/"));
        // POSIX-only flags must NOT appear for S3.
        assert!(!argv.iter().any(|a| a == "--direct"));
    }

    #[test]
    fn parse_csv_extracts_iops_and_throughput() {
        let tmp = TempDir::new().unwrap();
        let csv = tmp.path().join("run.csv");
        std::fs::write(
            &csv,
            "operation,IOPS [first],IOPS [last],MiB/s [first],MiB/s [last],\
             IO lat us min,IO lat us avg,IO lat us max\n\
             READ,18432,17900,18432.1,17900.0,12,612,7800\n\
             WRITE,820,790,820.5,790.1,210,1200,8200\n",
        )
        .unwrap();
        let rows = parse_csv(&csv).unwrap();
        assert_eq!(rows.len(), 2);
        let read = rows.iter().find(|r| r.operation_raw == "READ").unwrap();
        let phase = phase_from_row(read);
        assert_eq!(phase.operation, "read");
        assert_eq!(phase.iops_last, Some(17900.0));
        assert_eq!(phase.throughput_mib_s_first, Some(18432.1));
        assert_eq!(phase.io_lat_us.avg, Some(612.0));
    }

    #[test]
    fn parse_results_filters_mkdirs() {
        let tmp = TempDir::new().unwrap();
        let csv = tmp.path().join("run.csv");
        // Real elbencho-3.1-1 CSV header captured against WEKA.
        std::fs::write(
            &csv,
            "ISO date,label,path type,paths,hosts,threads,dirs,files,file size,\
             block size,direct IO,random,random aligned,IO depth,shared paths,truncate,\
             operation,time ms [first],time ms [last],entries/s [first],entries/s [last],\
             IOPS [first],IOPS [last],MiB/s [first],MiB/s [last],CPU% [first],CPU% [last],\
             entries [first],entries [last],MiB [first],MiB [last],\
             Ent lat us [min],Ent lat us [avg],Ent lat us [max],\
             IO lat us [min],IO lat us [avg],IO lat us [max]\n\
             2026-05-22T16:09:38,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,\
             MKDIRS,2,5,3277,1466,,,,,8,7,8,8,,,812,1068,1378,,,\n\
             2026-05-22T16:09:38,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,\
             WRITE,2134,2784,11,11,3494,2941,3494,2941,10,9,25,32,7460,8192,\
             290635,609112,867164,1661,9383,120903\n\
             2026-05-22T16:09:41,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,\
             READ,846,905,37,35,9673,9048,9673,9048,9,8,32,32,8192,8192,\
             49483,214645,404799,553,3223,6922\n",
        )
        .unwrap();
        let (phases, refs) = ElbenchoBackend::new()
            .parse_results(tmp.path(), "elbencho ...")
            .unwrap();
        assert!(!phases.contains_key("mkdirs"));
        assert!(phases.contains_key("read"));
        assert!(phases.contains_key("write"));
        assert_eq!(phases["read"].throughput_mib_s_last, Some(9048.0));
        assert_eq!(refs.command, "elbencho ...");
        assert!(refs.csv_path.as_deref().unwrap().contains("run.csv"));
    }

    #[test]
    fn service_command_format() {
        let backend = ElbenchoBackend::new();
        let client = ClientHost {
            elbencho_path: "/usr/bin/elbencho".into(),
            service_port: 1611,
            ..Default::default()
        };
        assert_eq!(
            backend.service_command(&client),
            vec!["/usr/bin/elbencho", "--service", "--port", "1611"]
        );
    }
}
