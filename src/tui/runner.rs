//! Event-driven runner the TUI reads from in a background thread.
//!
//! Mirrors python-legacy/src/elbencho_harness/tui/runner.py: yields one
//! event per phase / per spec as the benchmark walks the config's
//! materialized (SweepPoint, RunSpec) pairs.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use chrono::Utc;

use crate::backends::get_backend;
use crate::config::{loader, sweep, RunSpec};

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
    Failed(i32),
    Error,
}

impl SpecStatus {
    pub fn label(&self) -> String {
        match self {
            SpecStatus::Completed => "completed".into(),
            SpecStatus::Failed(rc) => format!("failed:{}", rc),
            SpecStatus::Error => "error".into(),
        }
    }
}

/// Walk a config end-to-end, posting events to the channel. Designed to run
/// in a background thread spawned from the TUI app.
pub fn execute(config: &Path, tx: Sender<RunEvent>) {
    if let Err(e) = execute_inner(config, &tx) {
        let _ = tx.send(RunEvent::Crashed {
            message: format!("{:#}", e),
        });
    }
}

fn execute_inner(config: &Path, tx: &Sender<RunEvent>) -> Result<()> {
    let plan = loader::load(config)?;
    let pairs = sweep::materialize_run_refs(&plan)?;
    if pairs.is_empty() {
        anyhow::bail!("config has neither `runs:` nor `sweeps:` to execute");
    }
    let backend = get_backend(plan.engine);
    let base_out = plan.output_dir.clone();
    std::fs::create_dir_all(&base_out)?;

    let first_label = match &pairs[0].0 {
        Some(point) => format!("sweep_{}", point.sweep_name),
        None => format!("{}_{}", pairs[0].1.target_name(), pairs[0].1.workload.name),
    };
    let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let slug = first_label
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

        let (status, report_path) =
            match crate::engine::run_spec(spec, &spec_dir, None, backend.as_ref()) {
                Ok(result) => {
                    let json = serde_json::to_string_pretty(&result)?;
                    std::fs::write(spec_dir.join("result.json"), json)?;
                    let report_path = spec_dir.join("report.html");
                    let _ = crate::report::render_single(&result, &report_path, None);
                    let st = if result.elbencho_exit_code == 0 {
                        completed += 1;
                        SpecStatus::Completed
                    } else {
                        failed += 1;
                        SpecStatus::Failed(result.elbencho_exit_code)
                    };
                    (st, Some(report_path))
                }
                Err(_) => {
                    failed += 1;
                    (SpecStatus::Error, None)
                }
            };
        let duration_s =
            (Utc::now() - started).num_milliseconds() as f64 / 1000.0;
        let _ = tx.send(RunEvent::SpecFinished {
            index: idx + 1,
            status,
            duration_s,
            report_path,
        });
    }
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

/// Dry-load a config and return its expanded (SweepPoint, RunSpec) pairs.
/// Used by the RunScreen to populate the table before launching.
pub fn plan_pairs(config: &Path) -> Result<Vec<(Option<sweep::SweepPoint>, RunSpec)>> {
    let plan = loader::load(config)?;
    sweep::materialize_run_refs(&plan)
}
