//! Compare report rendering (multi-run overlay).
//!
//! Loads N run directories from disk, aligns specs by (target, workload,
//! axis_values), emits one HTML with three overlay charts + a diff table
//! vs baseline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use askama::Template;
use indexmap::IndexMap;
use serde::Deserialize;

use super::charts::{axis_pretty_name, format_axis_value, overlay};
use crate::results::schema::Result as RunResult;

/// One result paired with the sweep axis values that produced it.
#[derive(Debug, Clone)]
pub struct ResultWithAxes {
    pub result: RunResult,
    pub axes: Option<IndexMap<String, serde_json::Value>>,
}

/// One loaded run directory ready for compare.
#[derive(Debug, Clone)]
pub struct LoadedRun {
    pub label: String,
    pub run_dir: PathBuf,
    pub results: Vec<ResultWithAxes>,
}

#[derive(Deserialize)]
struct ManifestFile {
    #[serde(default)]
    run_specs: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    spec_dir: String,
    #[serde(default)]
    axis_values: Option<IndexMap<String, serde_json::Value>>,
}

/// Load a run directory: read manifest.json if present, then each spec's
/// result.json. Each result is paired with its sweep axis values (from the
/// manifest entry) so two distinct sweep points with the same effective
/// spec_hash stay distinct in the compare table.
pub fn load_run(run_dir: &Path, label: Option<&str>) -> Result<LoadedRun> {
    let rdir = run_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", run_dir.display()))?;
    let mut paired: Vec<ResultWithAxes> = Vec::new();

    let manifest_path = rdir.join("manifest.json");
    let manifest: Option<ManifestFile> = if manifest_path.is_file() {
        let text = std::fs::read_to_string(&manifest_path)?;
        serde_json::from_str(&text).ok()
    } else {
        None
    };

    if let Some(mf) = manifest {
        for entry in mf.run_specs {
            let rj = rdir.join(&entry.spec_dir).join("result.json");
            if let Ok(text) = std::fs::read_to_string(&rj) {
                if let Ok(result) = serde_json::from_str::<RunResult>(&text) {
                    paired.push(ResultWithAxes {
                        result,
                        axes: entry.axis_values,
                    });
                }
            }
        }
    } else {
        // No manifest, walk subdirs.
        for entry in std::fs::read_dir(&rdir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let rj = entry.path().join("result.json");
            if let Ok(text) = std::fs::read_to_string(&rj) {
                if let Ok(result) = serde_json::from_str::<RunResult>(&text) {
                    paired.push(ResultWithAxes {
                        result,
                        axes: None,
                    });
                }
            }
        }
        // Stable order by dir name.
        paired.sort_by(|a, b| a.result.run_id.cmp(&b.result.run_id));
    }

    let label_str = label
        .map(String::from)
        .unwrap_or_else(|| rdir.file_name().unwrap_or_default().to_string_lossy().into_owned());
    Ok(LoadedRun {
        label: label_str,
        run_dir: rdir,
        results: paired,
    })
}

#[derive(Debug)]
struct CompareRow {
    #[allow(dead_code)]
    spec_key: (String, String, Vec<(String, serde_json::Value)>),
    target: String,
    workload: String,
    axis_label: String,
    axes: Option<IndexMap<String, serde_json::Value>>,
    per_run: HashMap<String, MetricSet>,
}

#[derive(Debug, Default, Clone, Copy)]
struct MetricSet {
    throughput: Option<f64>,
    iops: Option<f64>,
    avg_lat: Option<f64>,
}

fn primary_metrics(r: &RunResult) -> MetricSet {
    let phase = r
        .phases
        .get(&r.primary_phase)
        .or_else(|| r.phases.values().next());
    match phase {
        None => MetricSet::default(),
        Some(p) => MetricSet {
            throughput: p.throughput_mib_s_last.or(p.throughput_mib_s_first),
            iops: p.iops_last.or(p.iops_first),
            avg_lat: p.io_lat_us.avg,
        },
    }
}

fn spec_key(rwa: &ResultWithAxes) -> (String, String, Vec<(String, serde_json::Value)>) {
    let mut axes: Vec<(String, serde_json::Value)> = rwa
        .axes
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    axes.sort_by(|a, b| a.0.cmp(&b.0));
    (rwa.result.target.name.clone(), rwa.result.workload.name.clone(), axes)
}

fn format_axes_label(axes: &Option<IndexMap<String, serde_json::Value>>) -> String {
    let Some(m) = axes else { return String::new() };
    let mut parts: Vec<(String, &serde_json::Value)> =
        m.iter().map(|(k, v)| (k.clone(), v)).collect();
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    parts
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_rows(runs: &[LoadedRun]) -> Vec<CompareRow> {
    let mut keyed: HashMap<(String, String, Vec<(String, serde_json::Value)>), CompareRow> =
        HashMap::new();
    for run in runs {
        for rwa in &run.results {
            let key = spec_key(rwa);
            let row = keyed.entry(key.clone()).or_insert_with(|| CompareRow {
                spec_key: key.clone(),
                target: rwa.result.target.name.clone(),
                workload: rwa.result.workload.name.clone(),
                axis_label: format_axes_label(&rwa.axes),
                axes: rwa.axes.clone(),
                per_run: HashMap::new(),
            });
            row.per_run.insert(run.label.clone(), primary_metrics(&rwa.result));
        }
    }
    let common_axis = detect_common_axis(&keyed.values().map(|r| r.axes.clone()).collect::<Vec<_>>());
    let mut rows: Vec<CompareRow> = keyed.into_values().collect();
    if let Some(axis) = common_axis {
        rows.sort_by(|a, b| {
            let av = numeric_axis_value(&a.axes, &axis);
            let bv = numeric_axis_value(&b.axes, &axis);
            (a.target.clone(), a.workload.clone(), av).partial_cmp(&(
                b.target.clone(),
                b.workload.clone(),
                bv,
            )).unwrap()
        });
    } else {
        rows.sort_by(|a, b| {
            (a.target.clone(), a.workload.clone(), a.axis_label.clone()).cmp(&(
                b.target.clone(),
                b.workload.clone(),
                b.axis_label.clone(),
            ))
        });
    }
    rows
}

fn detect_common_axis(axes_dicts: &[Option<IndexMap<String, serde_json::Value>>]) -> Option<String> {
    let mut axis_name: Option<String> = None;
    for d in axes_dicts {
        let Some(d) = d else { return None };
        if d.len() != 1 {
            return None;
        }
        let only = d.keys().next().unwrap().clone();
        match &axis_name {
            None => axis_name = Some(only),
            Some(existing) => {
                if existing != &only {
                    return None;
                }
            }
        }
    }
    axis_name
}

fn numeric_axis_value(
    axes: &Option<IndexMap<String, serde_json::Value>>,
    axis: &str,
) -> f64 {
    let Some(map) = axes else { return f64::INFINITY };
    let Some(v) = map.get(axis) else { return f64::INFINITY };
    match v {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(f64::INFINITY),
        serde_json::Value::String(s) => s.parse().unwrap_or(f64::INFINITY),
        _ => f64::INFINITY,
    }
}

fn axis_for_chart(rows: &[CompareRow]) -> (Vec<String>, &'static str) {
    let common = detect_common_axis(&rows.iter().map(|r| r.axes.clone()).collect::<Vec<_>>());
    if let Some(axis) = &common {
        let labels: Vec<String> = rows
            .iter()
            .map(|r| {
                let v = numeric_axis_value(&r.axes, axis);
                if v.is_finite() {
                    format_axis_value(axis, v as i64)
                } else {
                    "".into()
                }
            })
            .collect();
        (labels, axis_pretty_name(axis))
    } else {
        let labels: Vec<String> = rows
            .iter()
            .map(|r| {
                let mut s = format!("{}·{}", r.target, r.workload);
                if !r.axis_label.is_empty() {
                    s.push('·');
                    s.push_str(&r.axis_label);
                }
                s
            })
            .collect();
        (labels, "")
    }
}

#[derive(Template)]
#[template(path = "compare.html")]
struct ComparePage {
    title: String,
    baseline: String,
    run_count: usize,
    runs: Vec<RunPill>,
    rows: Vec<RowView>,
    run_columns: Vec<RunColumn>,
    throughput_json: String,
    iops_json: String,
    latency_json: String,
}

struct RunPill {
    label: String,
    spec_count: usize,
    run_dir: String,
}

struct RowView {
    target: String,
    workload: String,
    axis_label: String,
    cells: Vec<RowCell>,
}

struct RowCell {
    value_fmt: String,
    show_delta: bool,
    delta_fmt: String,
    delta_class: String,
}

struct RunColumn {
    label: String,
    show_delta: bool,
}

/// Write a multi-run compare HTML to out_path. Aligns specs by
/// (target, workload, axis values); diff column shows percentage change
/// vs the baseline run.
pub fn render_compare(
    runs: &[LoadedRun],
    out_path: &Path,
    baseline_label: Option<&str>,
    title: &str,
) -> Result<()> {
    if runs.is_empty() {
        return Err(anyhow!("render_compare requires at least one LoadedRun"));
    }
    let baseline = baseline_label
        .map(String::from)
        .unwrap_or_else(|| runs[0].label.clone());

    let rows = build_rows(runs);
    let (x_labels, x_title) = axis_for_chart(&rows);

    let series_for = |sel: fn(&MetricSet) -> Option<f64>| -> Vec<(String, Vec<Option<f64>>)> {
        runs.iter()
            .map(|run| {
                let ys: Vec<Option<f64>> = rows
                    .iter()
                    .map(|r| r.per_run.get(&run.label).and_then(|m| sel(m)))
                    .collect();
                (run.label.clone(), ys)
            })
            .collect()
    };

    let throughput_fig = overlay(
        "Throughput",
        x_title,
        "MiB/s",
        "MiB/s",
        &x_labels,
        &series_for(|m| m.throughput),
    );
    let iops_fig = overlay(
        "IOPS",
        x_title,
        "IOPS",
        "IOPS",
        &x_labels,
        &series_for(|m| m.iops),
    );
    let latency_fig = overlay(
        "Average IO latency",
        x_title,
        "µs",
        "µs",
        &x_labels,
        &series_for(|m| m.avg_lat),
    );

    let run_pills: Vec<RunPill> = runs
        .iter()
        .map(|r| RunPill {
            label: r.label.clone(),
            spec_count: r.results.len(),
            run_dir: r.run_dir.display().to_string(),
        })
        .collect();
    let run_columns: Vec<RunColumn> = runs
        .iter()
        .map(|r| RunColumn {
            label: r.label.clone(),
            show_delta: r.label != baseline,
        })
        .collect();

    // Build the diff-table rows.
    let row_views: Vec<RowView> = rows
        .iter()
        .map(|row| {
            let base_tput = row
                .per_run
                .get(&baseline)
                .and_then(|m| m.throughput);
            let cells: Vec<RowCell> = runs
                .iter()
                .map(|run| {
                    let m = row.per_run.get(&run.label);
                    let v = m.and_then(|m| m.throughput);
                    let value_fmt = match v {
                        None => "—".into(),
                        Some(v) => format!("{:.1}", v),
                    };
                    let show_delta = run.label != baseline;
                    let (delta_fmt, delta_class) = if show_delta {
                        match (base_tput, v) {
                            (Some(b), Some(v)) if b != 0.0 => {
                                let d = (v - b) / b * 100.0;
                                let class = if d > 0.5 {
                                    "delta-pos"
                                } else if d < -0.5 {
                                    "delta-neg"
                                } else {
                                    "delta-zero"
                                };
                                let label = if d > 0.0 {
                                    format!("+{:.1}%", d)
                                } else {
                                    format!("{:.1}%", d)
                                };
                                (label, class.into())
                            }
                            _ => ("—".into(), "dim".into()),
                        }
                    } else {
                        (String::new(), String::new())
                    };
                    RowCell {
                        value_fmt,
                        show_delta,
                        delta_fmt,
                        delta_class,
                    }
                })
                .collect();
            RowView {
                target: row.target.clone(),
                workload: row.workload.clone(),
                axis_label: row.axis_label.clone(),
                cells,
            }
        })
        .collect();

    let page = ComparePage {
        title: title.into(),
        baseline,
        run_count: runs.len(),
        runs: run_pills,
        rows: row_views,
        run_columns,
        throughput_json: serde_json::to_string(&throughput_fig)?,
        iops_json: serde_json::to_string(&iops_fig)?,
        latency_json: serde_json::to_string(&latency_fig)?,
    };
    let html = page.render()?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, html)?;
    Ok(())
}
