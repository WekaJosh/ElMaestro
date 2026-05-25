//! Plotly.js JSON figure builders.
//!
//! Each function returns a JSON Value representing a complete Plotly figure
//! ({"data": [...], "layout": {...}}). The HTML templates embed these as
//! <script type="application/json"> and mount via Plotly.newPlot in the browser.
//!
//! Theme matches the Python version (GitHub dark palette).

use serde_json::{json, Value};

use crate::results::schema::Result as RunResult;

pub const PALETTE: &[&str] = &[
    "#58a6ff", "#3fb950", "#d29922", "#f85149", "#bc8cff", "#ff7b72", "#79c0ff", "#56d364",
];

const FONT_FAMILY: &str =
    "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, Roboto, 'Helvetica Neue', Arial, sans-serif";
const GRID: &str = "rgba(255,255,255,0.06)";
const AXIS: &str = "rgba(255,255,255,0.18)";
const FG: &str = "#e6edf3";
const MUTED: &str = "#8b949e";
const PANEL: &str = "#161b22";

/// Build a layout object with our standard dark theme.
fn theme_layout(title: &str, x_title: &str, y_title: &str, height: u32) -> Value {
    json!({
        "title": {
            "text": title,
            "x": 0.02,
            "y": 0.97,
            "font": {"size": 15, "color": FG, "family": FONT_FAMILY}
        },
        "plot_bgcolor": "rgba(0,0,0,0)",
        "paper_bgcolor": "rgba(0,0,0,0)",
        "font": {"family": FONT_FAMILY, "size": 12, "color": FG},
        "height": height,
        "margin": {"l": 60, "r": 24, "t": 70, "b": 60},
        "colorway": PALETTE,
        "legend": {
            "orientation": "h",
            "yanchor": "bottom",
            "y": 1.02,
            "xanchor": "left",
            "x": 0.02,
            "bgcolor": "rgba(0,0,0,0)",
            "font": {"size": 12, "color": FG}
        },
        "hovermode": "x unified",
        "hoverlabel": {
            "bgcolor": PANEL,
            "bordercolor": AXIS,
            "font": {"color": FG, "family": FONT_FAMILY, "size": 12}
        },
        "xaxis": {
            "title": {"text": x_title, "font": {"size": 12, "color": MUTED}},
            "gridcolor": GRID,
            "zerolinecolor": GRID,
            "linecolor": AXIS,
            "tickcolor": AXIS,
            "tickfont": {"color": FG, "size": 12},
            "ticks": "outside"
        },
        "yaxis": {
            "title": {"text": y_title, "font": {"size": 12, "color": MUTED}},
            "gridcolor": GRID,
            "zerolinecolor": GRID,
            "linecolor": AXIS,
            "tickcolor": AXIS,
            "tickfont": {"color": FG, "size": 12},
            "ticks": "outside",
            "rangemode": "tozero"
        },
        "barmode": "group",
        "bargap": 0.25
    })
}

/// Throughput chart for a single-spec report. Bars per phase.
pub fn single_throughput(result: &RunResult) -> Value {
    let phase_order = ["write", "read", "mixed"];
    let mut xs: Vec<&str> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for &name in &phase_order {
        if let Some(p) = result.phases.get(name) {
            xs.push(name);
            ys.push(p.throughput_mib_s_last.or(p.throughput_mib_s_first).unwrap_or(0.0));
        }
    }
    let label = format!("{}·{}", result.target.name, result.workload.name);
    let data = vec![json!({
        "type": "bar",
        "name": label,
        "x": xs,
        "y": ys,
        "marker": {"color": PALETTE[0]},
        "hovertemplate": "%{y:,.1f} MiB/s<extra>%{fullData.name}</extra>"
    })];
    json!({"data": data, "layout": theme_layout("Throughput by phase", "phase", "MiB/s", 420)})
}

/// Latency chart for a single-spec report. Min/avg/max bars per phase.
pub fn single_latency(result: &RunResult) -> Option<Value> {
    let mut xs: Vec<&str> = Vec::new();
    let mut mins: Vec<f64> = Vec::new();
    let mut avgs: Vec<f64> = Vec::new();
    let mut maxs: Vec<f64> = Vec::new();
    for (name, p) in &result.phases {
        xs.push(name);
        mins.push(p.io_lat_us.min.unwrap_or(0.0));
        avgs.push(p.io_lat_us.avg.unwrap_or(0.0));
        maxs.push(p.io_lat_us.max.unwrap_or(0.0));
    }
    if xs.is_empty() {
        return None;
    }
    let data = vec![
        json!({
            "type": "bar", "name": "min", "x": xs, "y": mins,
            "marker": {"color": PALETTE[1]},
            "hovertemplate": "%{y:,.0f} µs<extra>min</extra>"
        }),
        json!({
            "type": "bar", "name": "avg", "x": xs, "y": avgs,
            "marker": {"color": PALETTE[0]},
            "hovertemplate": "%{y:,.0f} µs<extra>avg</extra>"
        }),
        json!({
            "type": "bar", "name": "max", "x": xs, "y": maxs,
            "marker": {"color": PALETTE[3]},
            "hovertemplate": "%{y:,.0f} µs<extra>max</extra>"
        }),
    ];
    Some(json!({"data": data, "layout": theme_layout("IO latency (min / avg / max)", "phase", "µs", 420)}))
}

/// Multi-run overlay chart with both bar AND scatter+line trace pairs baked
/// in. Initial mode is bar (visible); the lines are hidden. The compare HTML
/// template's elbenchoToggleChart() flips visibility on click.
pub fn overlay(
    title: &str,
    x_axis_title: &str,
    y_title: &str,
    hover_unit: &str,
    x_labels: &[String],
    series: &[(String, Vec<Option<f64>>)],
) -> Value {
    let mut data: Vec<Value> = Vec::with_capacity(series.len() * 2);
    // Bars first.
    for (i, (name, ys)) in series.iter().enumerate() {
        let color = PALETTE[i % PALETTE.len()];
        let yvals: Vec<f64> = ys.iter().map(|v| v.unwrap_or(0.0)).collect();
        data.push(json!({
            "type": "bar",
            "name": name,
            "x": x_labels,
            "y": yvals,
            "visible": true,
            "marker": {"color": color, "line": {"width": 0}},
            "hovertemplate": format!(
                "%{{x}} • %{{y:,.1f}} {}<extra>{}</extra>", hover_unit, name
            )
        }));
    }
    // Scatter lines second (hidden initially).
    for (i, (name, ys)) in series.iter().enumerate() {
        let color = PALETTE[i % PALETTE.len()];
        let yvals: Vec<f64> = ys.iter().map(|v| v.unwrap_or(0.0)).collect();
        data.push(json!({
            "type": "scatter",
            "name": name,
            "x": x_labels,
            "y": yvals,
            "mode": "lines+markers",
            "visible": false,
            "line": {"color": color, "width": 2.5, "shape": "spline", "smoothing": 0.6},
            "marker": {"color": color, "size": 8, "line": {"width": 2, "color": "#0e1116"}},
            "hovertemplate": format!(
                "%{{x}} • %{{y:,.1f}} {}<extra>{}</extra>", hover_unit, name
            )
        }));
    }
    json!({"data": data, "layout": theme_layout(title, x_axis_title, y_title, 460)})
}

/// Format an axis value for chart x-tick labels.
///
/// block_size / dataset_size: bytes -> "64k", "1m", "4m" (lowercase)
/// rw_mix_pct_read:           "70%"
/// integers (threads, qd, ...): "8"
pub fn format_axis_value(axis: &str, value: i64) -> String {
    match axis {
        "block_size" | "dataset_size" => human_bytes(value as u64),
        "rw_mix_pct_read" => format!("{}%", value),
        _ => format!("{}", value),
    }
}

fn human_bytes(n: u64) -> String {
    for (unit, base) in [("g", 1u64 << 30), ("m", 1u64 << 20), ("k", 1u64 << 10)] {
        if n >= base && n % base == 0 {
            return format!("{}{}", n / base, unit);
        }
        if n >= base {
            return format!("{}{}", (n as f64) / (base as f64), unit);
        }
    }
    if n >= 1024 {
        format!("{}", n)
    } else {
        format!("{}b", n)
    }
}

pub fn axis_pretty_name(axis: &str) -> &'static str {
    match axis {
        "block_size" => "block size",
        "rw_mix_pct_read" => "read mix",
        "threads_per_client" => "threads / client",
        "io_depth" => "IO depth",
        "dataset_size" => "dataset size",
        "client_count" => "clients",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_canonical() {
        assert_eq!(human_bytes(65_536), "64k");
        assert_eq!(human_bytes(1_048_576), "1m");
        assert_eq!(human_bytes(4 * 1_048_576), "4m");
        assert_eq!(human_bytes(1 << 30), "1g");
    }

    #[test]
    fn axis_value_formatting() {
        assert_eq!(format_axis_value("block_size", 65_536), "64k");
        assert_eq!(format_axis_value("threads_per_client", 8), "8");
        assert_eq!(format_axis_value("rw_mix_pct_read", 70), "70%");
    }

    #[test]
    fn overlay_includes_bar_and_scatter_pairs() {
        let fig = overlay(
            "T",
            "x",
            "MiB/s",
            "MiB/s",
            &["64k".into(), "1m".into()],
            &[
                ("run-A".into(), vec![Some(100.0), Some(200.0)]),
                ("run-B".into(), vec![Some(110.0), Some(210.0)]),
            ],
        );
        let data = fig["data"].as_array().unwrap();
        assert_eq!(data.len(), 4);
        let bars: Vec<_> = data.iter().filter(|t| t["type"] == "bar").collect();
        let scatters: Vec<_> = data.iter().filter(|t| t["type"] == "scatter").collect();
        assert_eq!(bars.len(), 2);
        assert_eq!(scatters.len(), 2);
        // Bars visible, scatters hidden initially.
        assert_eq!(bars[0]["visible"], serde_json::Value::Bool(true));
        assert_eq!(scatters[0]["visible"], serde_json::Value::Bool(false));
    }
}
