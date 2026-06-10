//! Event-driven runner the TUI reads from in a background thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use chrono::Utc;

use crate::backends::get_backend;
use crate::config::{loader, sweep, RunPlan, RunSpec};

/// Metrics extracted from a finished spec's PhaseResult, formatted for live
/// display in the Run screen + the in-TUI report viewer. All fields are
/// optional because some engines / phases don't populate every field.
#[derive(Debug, Clone, Default)]
pub struct SpecMetrics {
    pub primary_phase: String,
    pub throughput_mib_s: Option<f64>,
    pub iops: Option<f64>,
    pub lat_avg_us: Option<f64>,
    pub lat_p50_us: Option<f64>,
    pub lat_p95_us: Option<f64>,
    pub lat_p99_us: Option<f64>,
    pub errors: u64,
}

impl SpecMetrics {
    pub fn from_result(r: &crate::results::schema::Result) -> Self {
        let phase = r.phases.get(&r.primary_phase);
        let mut m = SpecMetrics {
            primary_phase: r.primary_phase.clone(),
            ..Default::default()
        };
        if let Some(p) = phase {
            m.throughput_mib_s = p.throughput_mib_s_last.or(p.throughput_mib_s_first);
            m.iops = p.iops_last.or(p.iops_first);
            m.lat_avg_us = p.io_lat_us.avg;
            m.lat_p50_us = p.latency_percentiles_us.get("p50").copied();
            m.lat_p95_us = p.latency_percentiles_us.get("p95").copied();
            m.lat_p99_us = p.latency_percentiles_us.get("p99").copied();
            m.errors = p.errors;
        }
        m
    }
}

#[derive(Debug, Clone)]
pub enum RunEvent {
    RunStarted {
        run_dir: PathBuf,
        total: usize,
    },
    SpecPlanned {
        index: usize,
        target: String,
        workload: String,
        axis_label: String,
        spec_hash: String,
    },
    SpecStarted {
        index: usize,
    },
    SpecFinished {
        index: usize,
        status: SpecStatus,
        duration_s: f64,
        report_path: Option<PathBuf>,
        /// Path to result.json for the in-TUI report viewer to load.
        result_path: Option<PathBuf>,
        /// Headline metrics (throughput / IOPS / latency) for live display
        /// in the Run screen. `None` when the spec errored before producing
        /// a result.
        metrics: Option<SpecMetrics>,
    },
    RunFinished {
        run_dir: PathBuf,
        completed: usize,
        failed: usize,
    },
    Crashed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecStatus {
    Completed,
    /// Engine ran but exited non-zero. Carries the stderr snippet so
    /// the user can see why it failed without leaving the TUI (most
    /// common cause on multi-host: dataset path not present on one of
    /// the workers, ssh user without write permission on the mount,
    /// service-port collision, etc.).
    Failed(i32, String),
    /// Engine returned Err before producing a result. The string is the
    /// formatted anyhow chain (`{:#}`) so the TUI can show it to the
    /// user instead of just "error".
    Error(String),
}

impl SpecStatus {
    pub fn label(&self) -> String {
        match self {
            SpecStatus::Completed => "completed".into(),
            SpecStatus::Failed(rc, _) => format!("failed:{}", rc),
            SpecStatus::Error(_) => "error".into(),
        }
    }

    /// Long-form detail for the Run-screen footer when this row is
    /// highlighted. Some for both Failed and Error.
    pub fn error_detail(&self) -> Option<&str> {
        match self {
            SpecStatus::Error(msg) => Some(msg.as_str()),
            SpecStatus::Failed(_, msg) if !msg.is_empty() => Some(msg.as_str()),
            _ => None,
        }
    }
}

/// Execute a config file. Single iteration of the sweep.
pub fn execute(config: &Path, tx: Sender<RunEvent>) {
    let plan = match loader::load(config) {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(RunEvent::Crashed {
                message: format!("{:#}", e),
            });
            return;
        }
    };
    let label = config
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "run".into());
    execute_plan(plan, &label, 1, tx);
}

/// Execute an in-memory RunPlan, optionally repeating the whole sweep N times
/// for variance analysis. Each repetition lands in its own run directory.
pub fn execute_plan(mut plan: RunPlan, label: &str, repeats: usize, tx: Sender<RunEvent>) {
    // Backstop: every plan must pass the canonical finalize() pipeline
    // (brace expansion, engine-default repair, validation) before
    // execution. Both current sources (YAML loader, Configure form)
    // already finalize, and finalize is idempotent — this guarantees the
    // invariant holds for any future plan source too.
    if let Err(e) = plan.finalize() {
        let _ = tx.send(RunEvent::Crashed {
            message: format!("invalid plan: {:#}", e),
        });
        return;
    }
    let repeats = repeats.max(1);
    for iter in 0..repeats {
        let iter_label = if repeats > 1 {
            format!("{}_run{:02}", label, iter + 1)
        } else {
            label.to_string()
        };
        if let Err(e) = execute_one_iteration(&plan, &iter_label, &tx) {
            let _ = tx.send(RunEvent::Crashed {
                message: format!("{:#}", e),
            });
            return;
        }
    }
}

fn execute_one_iteration(plan: &RunPlan, label: &str, tx: &Sender<RunEvent>) -> Result<()> {
    let pairs = sweep::materialize_run_refs(plan)?;
    if pairs.is_empty() {
        anyhow::bail!("config has neither `runs:` nor `sweeps:` to execute");
    }
    let backend = get_backend(plan.engine);
    let base_out = plan.output_dir.clone();
    std::fs::create_dir_all(&base_out)?;

    let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let slug = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let run_dir = base_out.join(format!("{}_{}", ts, slug));
    std::fs::create_dir_all(&run_dir)?;

    let _ = tx.send(RunEvent::RunStarted {
        run_dir: run_dir.clone(),
        total: pairs.len(),
    });
    for (idx, (point, spec)) in pairs.iter().enumerate() {
        let axis_label = point.as_ref().map(|p| p.short_label()).unwrap_or_default();
        let _ = tx.send(RunEvent::SpecPlanned {
            index: idx + 1,
            target: spec.target_name().to_string(),
            workload: spec.workload.name.clone(),
            axis_label,
            spec_hash: spec.spec_hash.clone(),
        });
    }

    let mut completed = 0;
    let mut failed = 0;
    let mut manifest_entries: Vec<serde_json::Value> = Vec::new();
    let mut manifest_statuses: serde_json::Map<String, serde_json::Value> = Default::default();
    let manifest_run_id = ulid::Ulid::new().to_string();
    let manifest_created = Utc::now();

    for (idx, (point, spec)) in pairs.iter().enumerate() {
        let sweep_label = point.as_ref().map(|p| p.short_label());
        let spec_dir = make_spec_dir(
            &run_dir,
            idx + 1,
            spec.target_name(),
            &spec.workload.name,
            sweep_label.as_deref(),
        )?;
        let started = Utc::now();
        let _ = tx.send(RunEvent::SpecStarted { index: idx + 1 });

        let (status, report_path, result_path, metrics) =
            match crate::engine::run_spec(spec, &spec_dir, None, backend.as_ref()) {
                Ok(result) => {
                    let json = serde_json::to_string_pretty(&result)?;
                    let result_path = spec_dir.join("result.json");
                    std::fs::write(&result_path, json)?;
                    let report_path = spec_dir.join("report.html");
                    let _ = crate::report::render_single(&result, &report_path, None);
                    let st = if result.elbencho_exit_code == 0 {
                        completed += 1;
                        SpecStatus::Completed
                    } else {
                        failed += 1;
                        // result.errors already contains "elbencho
                        // exited N" + the stderr tail (see
                        // build_result). Join into one detail string so
                        // the Run screen footer renders the actual
                        // engine error when the user highlights a
                        // failed row.
                        let detail = result.errors.join(" | ");
                        SpecStatus::Failed(result.elbencho_exit_code, detail)
                    };
                    let metrics = SpecMetrics::from_result(&result);
                    (st, Some(report_path), Some(result_path), Some(metrics))
                }
                Err(e) => {
                    failed += 1;
                    // Capture the full anyhow chain so users see the
                    // actual reason in the Run screen instead of just
                    // "error". Common multi-host failures (ssh
                    // unreachable, service start timeout, port already
                    // bound) all surface here.
                    (SpecStatus::Error(format!("{:#}", e)), None, None, None)
                }
            };
        let duration_s = (Utc::now() - started).num_milliseconds() as f64 / 1000.0;
        let status_label = status.label();
        let _ = tx.send(RunEvent::SpecFinished {
            index: idx + 1,
            status,
            duration_s,
            report_path,
            result_path,
            metrics,
        });

        let axis_values: Option<serde_json::Value> = point.as_ref().map(|p| {
            let mut map = serde_json::Map::new();
            for (k, v) in &p.overrides {
                map.insert(k.clone(), v.to_json_value());
            }
            serde_json::Value::Object(map)
        });
        manifest_entries.push(serde_json::json!({
            "index": idx + 1,
            "spec_hash": spec.spec_hash,
            "run_id": spec.run_id,
            "target": spec.target_name(),
            "workload": spec.workload.name,
            "sweep": point.as_ref().map(|p| p.sweep_name.clone()),
            "axis_values": axis_values,
            "spec_dir": spec_dir
                .strip_prefix(&run_dir)
                .unwrap_or(&spec_dir)
                .display()
                .to_string(),
        }));
        manifest_statuses.insert(
            spec.spec_hash.clone(),
            serde_json::Value::String(status_label),
        );
    }

    let manifest = serde_json::json!({
        "schema_version": "1.0",
        "run_id": manifest_run_id,
        "created_at": manifest_created.to_rfc3339(),
        "run_specs": manifest_entries,
        "statuses": manifest_statuses,
    });
    let _ = std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    );

    let _ = tx.send(RunEvent::RunFinished {
        run_dir,
        completed,
        failed,
    });
    Ok(())
}

fn make_spec_dir(
    run_dir: &Path,
    index: usize,
    target: &str,
    workload: &str,
    label: Option<&str>,
) -> Result<PathBuf> {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let mut name = format!("{:04}_{}_{}", index, safe(target), safe(workload));
    if let Some(l) = label {
        if !l.is_empty() {
            name.push('_');
            name.push_str(&safe(l));
        }
    }
    let dir = run_dir.join(name);
    std::fs::create_dir_all(dir.join("raw"))?;
    Ok(dir)
}

/// Dry-load a config file and return its expanded pairs.
pub fn plan_pairs(config: &Path) -> Result<Vec<(Option<sweep::SweepPoint>, RunSpec)>> {
    let plan = loader::load(config)?;
    sweep::materialize_run_refs(&plan)
}

/// Expand an in-memory plan (used by the configure-screen flow).
pub fn plan_pairs_from(plan: &RunPlan) -> Result<Vec<(Option<sweep::SweepPoint>, RunSpec)>> {
    sweep::materialize_run_refs(plan)
}
