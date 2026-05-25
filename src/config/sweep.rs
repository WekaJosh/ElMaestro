//! Sweep -> list[(SweepPoint, RunSpec)] expansion.
//!
//! Mirrors python-legacy/src/elbencho_harness/config/sweep.py. Two orders:
//!
//!   cartesian: full cross-product of populated axes
//!   ladder:    vary one axis at a time, holding others at workload defaults
//!
//! `client_count` is special: doesn't override a workload field, instead
//! trims the client list to the first N entries.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use indexmap::IndexMap;

use super::{ClientHost, RunPlan, RunSpec, Sweep, Workload};

/// One materialized sweep point. Carries enough info to also be a unique
/// directory name + a row in the report.
#[derive(Debug, Clone)]
pub struct SweepPoint {
    pub sweep_name: String,
    pub target_name: String,
    /// Stable-ordered map of axis name -> raw value. Preserves the canonical
    /// axis order (block_size, rw_mix_pct_read, threads_per_client, io_depth,
    /// dataset_size, client_count).
    pub overrides: IndexMap<String, AxisValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisValue {
    Bytes(u64),
    Int(i64),
    Pct(u8),
}

impl AxisValue {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            AxisValue::Bytes(b) => Some(*b),
            AxisValue::Int(i) if *i >= 0 => Some(*i as u64),
            AxisValue::Pct(p) => Some(*p as u64),
            _ => None,
        }
    }
}

impl SweepPoint {
    /// Short human label suitable for directory names + chart x-axis ticks.
    /// e.g. "bs=64KiB", "t=4", "bs=1MiB_t=4".
    pub fn short_label(&self) -> String {
        if self.overrides.is_empty() {
            return "base".into();
        }
        self.overrides
            .iter()
            .map(|(k, v)| format!("{}={}", short_key(k), short_val(k, v)))
            .collect::<Vec<_>>()
            .join("_")
    }
}

fn short_key(k: &str) -> &str {
    match k {
        "block_size" => "bs",
        "rw_mix_pct_read" => "rwread",
        "threads_per_client" => "t",
        "io_depth" => "qd",
        "dataset_size" => "ds",
        "client_count" => "n",
        other => other,
    }
}

fn short_val(k: &str, v: &AxisValue) -> String {
    if k == "block_size" || k == "dataset_size" {
        return human_bytes(v.as_u64().unwrap_or(0));
    }
    match v {
        AxisValue::Bytes(b) => format!("{}", b),
        AxisValue::Int(i) => format!("{}", i),
        AxisValue::Pct(p) => format!("{}", p),
    }
}

/// 65536 -> "64KiB". Matches the Python `_human_bytes`.
fn human_bytes(n: u64) -> String {
    for (unit, base) in [("GiB", 1u64 << 30), ("MiB", 1u64 << 20), ("KiB", 1u64 << 10)] {
        if n % base == 0 && n >= base {
            return format!("{}{}", n / base, unit);
        }
    }
    format!("{}B", n)
}

/// Canonical axis order. Stable across runs so spec directory layout doesn't
/// drift between invocations.
const AXIS_ORDER: &[&str] = &[
    "block_size",
    "rw_mix_pct_read",
    "threads_per_client",
    "io_depth",
    "dataset_size",
    "client_count",
];

fn axis_iter(sweep: &Sweep) -> IndexMap<String, Vec<AxisValue>> {
    let mut out: IndexMap<String, Vec<AxisValue>> = IndexMap::new();
    let a = &sweep.axes;
    if let Some(vs) = &a.block_size {
        if !vs.is_empty() {
            out.insert("block_size".into(), vs.iter().map(|v| AxisValue::Bytes(*v)).collect());
        }
    }
    if let Some(vs) = &a.rw_mix_pct_read {
        if !vs.is_empty() {
            out.insert(
                "rw_mix_pct_read".into(),
                vs.iter().map(|v| AxisValue::Pct(*v)).collect(),
            );
        }
    }
    if let Some(vs) = &a.threads_per_client {
        if !vs.is_empty() {
            out.insert(
                "threads_per_client".into(),
                vs.iter().map(|v| AxisValue::Int(*v as i64)).collect(),
            );
        }
    }
    if let Some(vs) = &a.io_depth {
        if !vs.is_empty() {
            out.insert(
                "io_depth".into(),
                vs.iter().map(|v| AxisValue::Int(*v as i64)).collect(),
            );
        }
    }
    if let Some(vs) = &a.dataset_size {
        if !vs.is_empty() {
            out.insert(
                "dataset_size".into(),
                vs.iter().map(|v| AxisValue::Bytes(*v)).collect(),
            );
        }
    }
    if let Some(vs) = &a.client_count {
        if !vs.is_empty() {
            out.insert(
                "client_count".into(),
                vs.iter().map(|v| AxisValue::Int(*v as i64)).collect(),
            );
        }
    }
    // Reorder by canonical AXIS_ORDER (defensive: IndexMap preserves
    // insertion but the order above already follows AXIS_ORDER. This keeps
    // the contract explicit).
    let mut ordered = IndexMap::new();
    for name in AXIS_ORDER {
        if let Some(values) = out.shift_remove(*name) {
            ordered.insert((*name).into(), values);
        }
    }
    ordered
}

fn expand_cartesian(
    axes: &IndexMap<String, Vec<AxisValue>>,
) -> Vec<IndexMap<String, AxisValue>> {
    if axes.is_empty() {
        return vec![IndexMap::new()];
    }
    let names: Vec<&String> = axes.keys().collect();
    let value_lists: Vec<&Vec<AxisValue>> = names.iter().map(|n| &axes[*n]).collect();
    let mut out: Vec<IndexMap<String, AxisValue>> = vec![IndexMap::new()];
    for (i, values) in value_lists.iter().enumerate() {
        let mut next: Vec<IndexMap<String, AxisValue>> = Vec::with_capacity(out.len() * values.len());
        for partial in &out {
            for v in *values {
                let mut p = partial.clone();
                p.insert(names[i].clone(), v.clone());
                next.push(p);
            }
        }
        out = next;
    }
    out
}

fn expand_ladder(axes: &IndexMap<String, Vec<AxisValue>>) -> Vec<IndexMap<String, AxisValue>> {
    if axes.is_empty() {
        return vec![IndexMap::new()];
    }
    let mut out = Vec::new();
    for (name, values) in axes {
        for v in values {
            let mut combo = IndexMap::new();
            combo.insert(name.clone(), v.clone());
            out.push(combo);
        }
    }
    out
}

/// Expand one Sweep against its RunPlan into a list of SweepPoints.
pub fn expand(plan: &RunPlan, sweep: &Sweep) -> Result<Vec<SweepPoint>> {
    plan.workload_by_name(&sweep.base)
        .map_err(|_| anyhow!("sweep {:?} references unknown base workload: {}", sweep.name, sweep.base))?;

    let sweep_targets: Vec<&str> = sweep
        .targets
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_else(|| {
            sweep
                .target
                .as_ref()
                .map(|t| vec![t.as_str()])
                .unwrap_or_default()
        });
    if sweep_targets.is_empty() {
        anyhow::bail!("sweep {:?} has no targets", sweep.name);
    }

    let axes = axis_iter(sweep);
    let combos = match sweep.order.as_str() {
        "ladder" => expand_ladder(&axes),
        _ => expand_cartesian(&axes),
    };

    let mut points = Vec::new();
    for target_name in sweep_targets {
        for combo in &combos {
            points.push(SweepPoint {
                sweep_name: sweep.name.clone(),
                target_name: target_name.into(),
                overrides: combo.clone(),
            });
            if let Some(max) = sweep.max_runs {
                if points.len() >= max {
                    return Ok(points);
                }
            }
        }
    }
    Ok(points)
}

/// Materialize a SweepPoint into a concrete RunSpec.
pub fn materialize(plan: &RunPlan, point: &SweepPoint) -> Result<RunSpec> {
    let target = plan.target_by_name(&point.target_name)?.clone();
    let base_wl = base_workload_for_sweep(plan, &point.sweep_name)?;
    let workload = apply_workload_overrides(base_wl, &point.overrides);
    let clients = apply_client_count(plan.clients.clone(), &point.overrides)?;
    let spec_hash = RunSpec::make_spec_hash(&target, &workload, &clients);
    let run_id = ulid::Ulid::new().to_string();
    Ok(RunSpec {
        run_id,
        spec_hash,
        target,
        workload,
        clients,
    })
}

fn base_workload_for_sweep<'a>(plan: &'a RunPlan, sweep_name: &str) -> Result<&'a Workload> {
    let sw = plan
        .sweeps
        .iter()
        .find(|s| s.name == sweep_name)
        .ok_or_else(|| anyhow!("sweep not found: {}", sweep_name))?;
    plan.workload_by_name(&sw.base)
}

fn apply_workload_overrides(
    base: &Workload,
    overrides: &IndexMap<String, AxisValue>,
) -> Workload {
    let mut out = base.clone();
    for (k, v) in overrides {
        match k.as_str() {
            "block_size" => {
                if let Some(b) = v.as_u64() {
                    out.block_size = b;
                }
            }
            "rw_mix_pct_read" => {
                if let AxisValue::Pct(p) = v {
                    out.rw_mix_pct_read = *p;
                }
            }
            "threads_per_client" => {
                if let Some(n) = v.as_u64() {
                    out.threads_per_client = n as u32;
                }
            }
            "io_depth" => {
                if let Some(n) = v.as_u64() {
                    out.io_depth = n as u32;
                }
            }
            "dataset_size" => {
                if let Some(b) = v.as_u64() {
                    out.dataset_size = Some(b);
                }
            }
            _ => {}
        }
    }
    out
}

fn apply_client_count(
    clients: Vec<ClientHost>,
    overrides: &IndexMap<String, AxisValue>,
) -> Result<Vec<ClientHost>> {
    if let Some(v) = overrides.get("client_count") {
        let n = v.as_u64().ok_or_else(|| anyhow!("client_count not an integer"))? as usize;
        if n > clients.len() {
            anyhow::bail!(
                "sweep axis client_count={} exceeds available clients ({})",
                n,
                clients.len()
            );
        }
        return Ok(clients.into_iter().take(n).collect());
    }
    Ok(clients)
}

/// Combine `runs:` entries with expanded sweeps, in declaration order.
/// `runs:` come first (with point=None), then each sweep in declaration order.
pub fn materialize_run_refs(plan: &RunPlan) -> Result<Vec<(Option<SweepPoint>, RunSpec)>> {
    let mut out = Vec::new();
    let clients = if plan.clients.is_empty() {
        vec![ClientHost::default()]
    } else {
        plan.clients.clone()
    };
    for r in &plan.runs {
        let target = plan.target_by_name(&r.target)?.clone();
        let workload = plan.workload_by_name(&r.workload)?.clone();
        let spec_hash = RunSpec::make_spec_hash(&target, &workload, &clients);
        out.push((
            None,
            RunSpec {
                run_id: ulid::Ulid::new().to_string(),
                spec_hash,
                target,
                workload,
                clients: clients.clone(),
            },
        ));
    }
    for sw in &plan.sweeps {
        for point in expand(plan, sw)? {
            let spec = materialize(plan, &point)?;
            out.push((Some(point), spec));
        }
    }
    Ok(out)
}

// Cargo doesn't like unused: just suppress for the dev tools.
#[allow(dead_code)]
fn _unused_anchor() {
    let _: BTreeMap<&str, ()> = BTreeMap::new();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PosixTarget, SweepAxis, Target, Workload};

    fn base_plan() -> RunPlan {
        RunPlan {
            version: 1,
            engine: crate::config::Engine::Elbencho,
            output_dir: "./results".into(),
            clients: vec![ClientHost::default()],
            targets: vec![Target::Posix(PosixTarget {
                name: "t".into(),
                mount_path: "/mnt".into(),
                dataset_subdir: "bench".into(),
                cleanup: false,
            })],
            workloads: vec![Workload {
                name: "base".into(),
                pattern: "seq".into(),
                rw_mix_pct_read: 100,
                block_size: 4096,
                threads_per_client: 2,
                io_depth: 1,
                direct_io: false,
                sync_after_write: false,
                drop_caches_before: false,
                duration_s: None,
                dataset_size: None,
                file_size: Some(4096),
                file_count: None,
                s3_multipart_size: None,
                s3_object_prefix: None,
                extra_flags: vec![],
            }],
            runs: vec![],
            sweeps: vec![],
        }
    }

    fn plan_with_sweep(axes: SweepAxis, order: &str, max_runs: Option<usize>) -> RunPlan {
        let mut plan = base_plan();
        plan.sweeps.push(Sweep {
            name: "sw".into(),
            base: "base".into(),
            targets: None,
            target: Some("t".into()),
            axes,
            order: order.into(),
            max_runs,
        });
        plan
    }

    #[test]
    fn cartesian_full_product() {
        let plan = plan_with_sweep(
            SweepAxis {
                block_size: Some(vec![4096, 1048576]),
                threads_per_client: Some(vec![2, 4, 8]),
                ..Default::default()
            },
            "cartesian",
            None,
        );
        let points = expand(&plan, &plan.sweeps[0]).unwrap();
        assert_eq!(points.len(), 6); // 2 * 3
    }

    #[test]
    fn ladder_one_axis_at_a_time() {
        let plan = plan_with_sweep(
            SweepAxis {
                block_size: Some(vec![4096, 1048576]),
                threads_per_client: Some(vec![2, 4, 8]),
                ..Default::default()
            },
            "ladder",
            None,
        );
        let points = expand(&plan, &plan.sweeps[0]).unwrap();
        assert_eq!(points.len(), 5); // 2 + 3
    }

    #[test]
    fn max_runs_caps_expansion() {
        let plan = plan_with_sweep(
            SweepAxis {
                block_size: Some(vec![4096, 1048576]),
                threads_per_client: Some(vec![2, 4, 8]),
                ..Default::default()
            },
            "cartesian",
            Some(4),
        );
        let points = expand(&plan, &plan.sweeps[0]).unwrap();
        assert_eq!(points.len(), 4);
    }

    #[test]
    fn ladder_order_is_deterministic() {
        let plan = plan_with_sweep(
            SweepAxis {
                threads_per_client: Some(vec![2, 4]),
                block_size: Some(vec![4096, 8192]),
                io_depth: Some(vec![1, 2]),
                ..Default::default()
            },
            "ladder",
            None,
        );
        let first = expand(&plan, &plan.sweeps[0]).unwrap();
        let second = expand(&plan, &plan.sweeps[0]).unwrap();
        let first_axes: Vec<String> = first
            .iter()
            .map(|p| p.overrides.keys().next().unwrap().clone())
            .collect();
        let second_axes: Vec<String> = second
            .iter()
            .map(|p| p.overrides.keys().next().unwrap().clone())
            .collect();
        assert_eq!(first_axes, second_axes);
        // Canonical axis order: block_size first, then threads_per_client, then io_depth.
        let mut seen: Vec<String> = Vec::new();
        for a in &first_axes {
            if !seen.contains(a) {
                seen.push(a.clone());
            }
        }
        assert_eq!(seen, vec!["block_size", "threads_per_client", "io_depth"]);
    }

    #[test]
    fn short_label_humanizes_bytes() {
        let mut overrides = IndexMap::new();
        overrides.insert("block_size".into(), AxisValue::Bytes(1048576));
        overrides.insert("threads_per_client".into(), AxisValue::Int(4));
        let p = SweepPoint {
            sweep_name: "sw".into(),
            target_name: "t".into(),
            overrides,
        };
        let s = p.short_label();
        assert!(s.contains("bs=1MiB"), "{}", s);
        assert!(s.contains("t=4"), "{}", s);
    }

    #[test]
    fn short_label_empty_is_base() {
        let p = SweepPoint {
            sweep_name: "sw".into(),
            target_name: "t".into(),
            overrides: IndexMap::new(),
        };
        assert_eq!(p.short_label(), "base");
    }

    #[test]
    fn materialize_overrides_workload_block_size() {
        let plan = plan_with_sweep(
            SweepAxis {
                block_size: Some(vec![1048576]),
                ..Default::default()
            },
            "cartesian",
            None,
        );
        let point = expand(&plan, &plan.sweeps[0]).unwrap().pop().unwrap();
        let spec = materialize(&plan, &point).unwrap();
        assert_eq!(spec.workload.block_size, 1048576);
        // Other fields preserved.
        assert_eq!(spec.workload.threads_per_client, 2);
    }

    #[test]
    fn client_count_trims_client_list() {
        let mut plan = plan_with_sweep(
            SweepAxis {
                client_count: Some(vec![1, 2]),
                ..Default::default()
            },
            "cartesian",
            None,
        );
        plan.clients.push(ClientHost {
            host: "h2".into(),
            ssh_user: Some("u".into()),
            ..Default::default()
        });
        plan.clients.push(ClientHost {
            host: "h3".into(),
            ssh_user: Some("u".into()),
            ..Default::default()
        });
        let points = expand(&plan, &plan.sweeps[0]).unwrap();
        let counts: Vec<usize> = points
            .iter()
            .map(|p| materialize(&plan, p).unwrap().clients.len())
            .collect();
        assert_eq!(counts, vec![1, 2]);
    }

    #[test]
    fn client_count_too_high_errors() {
        let mut plan = plan_with_sweep(
            SweepAxis {
                client_count: Some(vec![5]),
                ..Default::default()
            },
            "cartesian",
            None,
        );
        plan.clients.push(ClientHost {
            host: "h2".into(),
            ssh_user: Some("u".into()),
            ..Default::default()
        });
        let p = expand(&plan, &plan.sweeps[0]).unwrap().pop().unwrap();
        let err = materialize(&plan, &p).unwrap_err();
        assert!(format!("{:#}", err).contains("exceeds available clients"));
    }

    #[test]
    fn expand_with_no_axes_emits_single_baseline() {
        let plan = plan_with_sweep(SweepAxis::default(), "cartesian", None);
        let points = expand(&plan, &plan.sweeps[0]).unwrap();
        assert_eq!(points.len(), 1);
        assert!(points[0].overrides.is_empty());
    }
}
