//! Single-spec report rendering. Builds headline tiles + Plotly.js JSON
//! and renders into the askama single.html template.

use std::path::Path;

use anyhow::Result;
use askama::Template;

use crate::results::schema::Result as RunResult;

use super::charts::{single_latency, single_throughput};

#[derive(askama::Template)]
#[template(path = "single.html")]
struct SinglePage {
    run_label: String,
    engine: String,
    schema_version: String,
    run_id: String,
    spec_hash: String,
    started_at: String,
    finished_at: String,
    duration_s_fmt: String,
    exit_code: i32,
    target_kind: String,
    target_name: String,
    workload_name: String,
    primary_phase: String,
    block_size: u64,
    threads_per_client: u32,
    io_depth: u32,
    pattern: String,
    rw_mix_pct_read: u8,
    direct_io: bool,
    total_concurrency: u64,
    command: String,
    tiles: Vec<Tile>,
    throughput_json: String,
    latency_json: String,
    has_latency: bool,
}

#[derive(Clone)]
struct Tile {
    label: String,
    formatted: String,
    unit: String,
    subtle: bool,
}

/// Render one Result to a self-contained HTML file. Returns the path written.
pub fn render_single(result: &RunResult, out_path: &Path, run_label: Option<&str>) -> Result<()> {
    let tiles = headline_tiles(result);

    let throughput_json = serde_json::to_string(&single_throughput(result))?;
    let latency_fig = single_latency(result);
    let has_latency = latency_fig.is_some();
    let latency_json = latency_fig
        .map(|v| serde_json::to_string(&v))
        .transpose()?
        .unwrap_or_default();

    let label = run_label
        .map(String::from)
        .unwrap_or_else(|| format!("{} · {}", result.target.name, result.workload.name));

    let page = SinglePage {
        run_label: label,
        engine: result.engine.clone(),
        schema_version: result.schema_version.clone(),
        run_id: result.run_id.clone(),
        spec_hash: result.spec_hash.clone(),
        started_at: result.started_at.to_rfc3339(),
        finished_at: result.finished_at.to_rfc3339(),
        duration_s_fmt: format!("{:.2}", result.duration_s),
        exit_code: result.elbencho_exit_code,
        target_kind: result.target.kind.clone(),
        target_name: result.target.name.clone(),
        workload_name: result.workload.name.clone(),
        primary_phase: result.primary_phase.clone(),
        block_size: result.workload.block_size,
        threads_per_client: result.workload.threads_per_client,
        io_depth: result.workload.io_depth,
        pattern: result.workload.pattern.clone(),
        rw_mix_pct_read: result.workload.rw_mix_pct_read,
        direct_io: result.workload.direct_io,
        total_concurrency: result.workload.total_concurrency,
        command: result.elbencho.command.clone(),
        tiles,
        throughput_json,
        latency_json,
        has_latency,
    };

    let html = page.render()?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, html)?;
    Ok(())
}

fn headline_tiles(result: &RunResult) -> Vec<Tile> {
    let phase = result
        .phases
        .get(&result.primary_phase)
        .or_else(|| result.phases.values().next());
    let mut tiles: Vec<Tile> = Vec::new();
    let Some(phase) = phase else { return tiles };

    let throughput = phase
        .throughput_mib_s_last
        .or(phase.throughput_mib_s_first);
    let iops = phase.iops_last.or(phase.iops_first);
    let ops = phase.ops_per_s_last.or(phase.ops_per_s_first);

    tiles.push(Tile {
        label: "Throughput".into(),
        formatted: fmt_num(throughput, ",.1"),
        unit: "MiB/s".into(),
        subtle: false,
    });
    if let Some(_) = iops {
        tiles.push(Tile {
            label: "IOPS".into(),
            formatted: fmt_num(iops, ",.0"),
            unit: "".into(),
            subtle: false,
        });
    }
    if let Some(_) = ops {
        tiles.push(Tile {
            label: "Ops/s".into(),
            formatted: fmt_num(ops, ",.0"),
            unit: "".into(),
            subtle: false,
        });
    }
    tiles.push(Tile {
        label: "Avg latency".into(),
        formatted: fmt_num(phase.io_lat_us.avg, ",.1"),
        unit: "µs".into(),
        subtle: false,
    });
    tiles.push(Tile {
        label: "Max latency".into(),
        formatted: fmt_num(phase.io_lat_us.max, ",.1"),
        unit: "µs".into(),
        subtle: false,
    });
    tiles.push(Tile {
        label: "CPU".into(),
        formatted: fmt_num(phase.cpu_pct, ".1"),
        unit: "%".into(),
        subtle: false,
    });
    tiles.push(Tile {
        label: "Errors".into(),
        formatted: format!("{}", phase.errors),
        unit: "".into(),
        subtle: phase.errors == 0,
    });
    tiles.push(Tile {
        label: "Duration".into(),
        formatted: format!("{:.1}", result.duration_s),
        unit: "s".into(),
        subtle: true,
    });
    tiles
}

/// Lightweight number formatter. spec ~= Python's ",.1f" / ",.0f" / ".1f".
fn fmt_num(value: Option<f64>, spec: &str) -> String {
    let Some(v) = value else { return "—".into() };
    let comma = spec.starts_with(',');
    let precision: usize = spec
        .rsplit('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let formatted = format!("{:.*}", precision, v);
    if comma {
        insert_thousands_commas(&formatted)
    } else {
        formatted
    }
}

fn insert_thousands_commas(s: &str) -> String {
    let (int_part, rest) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let (sign, digits) = if let Some(rest_digits) = int_part.strip_prefix('-') {
        ("-", rest_digits)
    } else {
        ("", int_part)
    };
    let chars: Vec<char> = digits.chars().rev().collect();
    let with_commas: String = chars
        .chunks(3)
        .map(|c| c.iter().rev().collect::<String>())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(",");
    match rest {
        Some(r) => format!("{}{}.{}", sign, with_commas, r),
        None => format!("{}{}", sign, with_commas),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_with_commas() {
        assert_eq!(fmt_num(Some(1234.567), ",.1"), "1,234.6");
        assert_eq!(fmt_num(Some(1_000_000.0), ",.0"), "1,000,000");
    }

    #[test]
    fn fmt_num_dash_for_none() {
        assert_eq!(fmt_num(None, ",.1"), "—");
    }

    #[test]
    fn fmt_num_no_comma() {
        assert_eq!(fmt_num(Some(42.5), ".1"), "42.5");
    }
}
