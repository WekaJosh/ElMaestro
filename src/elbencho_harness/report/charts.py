"""Plotly figure factories with a polished dark theme + interactive Bar/Line toggle.

Public API:
  - headline_tiles(result)                          one-result tile dicts
  - throughput_per_phase([results])                 single-spec phase bars
  - latency_overview([results])                     single-spec lat min/avg/max
  - sweep_overlay(...)                              multi-series overlay
                                                    with Bar/Line toggle
  - render_figure_html(fig)                         render to plain HTML

Design notes:
- ``_apply_theme`` puts every figure on the same dark visual language as the
  page (transparent backgrounds, soft gridlines, GitHub-style palette).
- ``_format_axis_value`` formats bytes ("64k", "1m"), counts ("8"), percents
  ("70%"), so x-tick labels stay terse.
- ``_detect_common_axis`` decides whether the sweep is one-dimensional (in
  which case the x-axis can be a clean numeric scale labeled by axis value)
  or mixed (fall back to per-point composite labels).
"""

from __future__ import annotations

from typing import Iterable, Sequence

import plotly.graph_objects as go

from ..results.schema import PhaseResult, Result


# ---------------------------------------------------------------------------
# Theme + colors
# ---------------------------------------------------------------------------

# GitHub-style dark palette, accessible (WCAG AA on #0e1116 background).
PALETTE = [
    "#58a6ff",  # blue
    "#3fb950",  # green
    "#d29922",  # amber
    "#f85149",  # red
    "#bc8cff",  # purple
    "#ff7b72",  # coral
    "#79c0ff",  # light blue
    "#56d364",  # light green
]

_FONT_FAMILY = (
    "-apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, "
    "Roboto, 'Helvetica Neue', Arial, sans-serif"
)
_GRID = "rgba(255,255,255,0.06)"
_AXIS = "rgba(255,255,255,0.18)"
_FG = "#e6edf3"
_MUTED = "#8b949e"
_PANEL = "#161b22"


def _apply_theme(
    fig: go.Figure,
    *,
    title: str | None = None,
    x_title: str | None = None,
    y_title: str | None = None,
    height: int = 460,
) -> None:
    """Apply the project's dark theme in place."""
    fig.update_layout(
        title=dict(
            text=title or "",
            x=0.02,
            y=0.97,
            font=dict(size=15, color=_FG, family=_FONT_FAMILY, weight=600),
        ),
        plot_bgcolor="rgba(0,0,0,0)",
        paper_bgcolor="rgba(0,0,0,0)",
        font=dict(family=_FONT_FAMILY, size=12, color=_FG),
        height=height,
        margin=dict(l=60, r=24, t=70, b=60),
        colorway=PALETTE,
        legend=dict(
            orientation="h",
            yanchor="bottom",
            y=1.02,
            xanchor="left",
            x=0.02,
            bgcolor="rgba(0,0,0,0)",
            font=dict(size=12, color=_FG),
        ),
        hovermode="x unified",
        hoverlabel=dict(
            bgcolor=_PANEL,
            bordercolor=_AXIS,
            font=dict(color=_FG, family=_FONT_FAMILY, size=12),
        ),
    )
    fig.update_xaxes(
        title=dict(text=x_title or "", font=dict(size=12, color=_MUTED)),
        gridcolor=_GRID,
        zerolinecolor=_GRID,
        linecolor=_AXIS,
        tickcolor=_AXIS,
        tickfont=dict(color=_FG, size=12),
        ticks="outside",
    )
    fig.update_yaxes(
        title=dict(text=y_title or "", font=dict(size=12, color=_MUTED)),
        gridcolor=_GRID,
        zerolinecolor=_GRID,
        linecolor=_AXIS,
        tickcolor=_AXIS,
        tickfont=dict(color=_FG, size=12),
        ticks="outside",
        rangemode="tozero",
    )


def _toggle_buttons(*, n_series: int) -> list[dict]:
    """Update-menu buttons that flip between Bar and Line trace visibility.

    The figure must contain 2*n_series traces: bars (visible) followed by
    scatter-lines (hidden). Each button rewrites the `visible` array.
    """
    bar_visible = [True] * n_series + [False] * n_series
    line_visible = [False] * n_series + [True] * n_series
    return [
        dict(
            type="buttons",
            direction="right",
            x=1.0,
            xanchor="right",
            y=1.16,
            yanchor="top",
            pad=dict(t=2, r=2, b=2, l=2),
            showactive=True,
            active=0,
            bgcolor=_PANEL,
            bordercolor=_AXIS,
            font=dict(color=_FG, size=11, family=_FONT_FAMILY),
            buttons=[
                dict(label="Bar", method="update", args=[{"visible": bar_visible}]),
                dict(label="Line", method="update", args=[{"visible": line_visible}]),
            ],
        )
    ]


# ---------------------------------------------------------------------------
# Value formatting
# ---------------------------------------------------------------------------


def _human_bytes(n: int) -> str:
    """65536 -> '64k', 1048576 -> '1m', 1073741824 -> '1g'. Lowercase.

    Uses binary units (1024-based) without the 'i' suffix to keep labels terse.
    Sub-1k values render with a 'b' suffix so the axis stays uniform.
    """
    n = int(n)
    for unit, base in (("g", 1024 ** 3), ("m", 1024 ** 2), ("k", 1024)):
        if n >= base and n % base == 0:
            return f"{n // base}{unit}"
        if n >= base:
            v = n / base
            return f"{v:g}{unit}"
    return f"{n}b" if n < 1024 else str(n)


def format_axis_value(axis: str, value) -> str:
    """Format one sweep-axis value for an x-tick label.

    block_size / dataset_size: bytes ('64k', '4m')
    rw_mix_pct_read:           percent ('70%')
    everything else:           str(value) ('8', '16')
    """
    if value is None:
        return ""
    if axis in {"block_size", "dataset_size"}:
        return _human_bytes(value)
    if axis == "rw_mix_pct_read":
        return f"{int(value)}%"
    return str(value)


def axis_pretty_name(axis: str) -> str:
    return {
        "block_size": "block size",
        "rw_mix_pct_read": "read mix",
        "threads_per_client": "threads / client",
        "io_depth": "IO depth",
        "dataset_size": "dataset size",
        "client_count": "clients",
    }.get(axis, axis)


def detect_common_axis(axes_dicts: Sequence[dict | None]) -> str | None:
    """If every dict has exactly one key and they're all the same key, return it.

    Otherwise None (caller falls back to composite labels).
    """
    axis_names: set[str] = set()
    for d in axes_dicts:
        if not d:
            return None
        keys = list(d.keys())
        if len(keys) != 1:
            return None
        axis_names.add(keys[0])
    if len(axis_names) == 1:
        return axis_names.pop()
    return None


# ---------------------------------------------------------------------------
# Single-spec charts (used by render_single)
# ---------------------------------------------------------------------------


def _safe(v):
    return v if v is not None else 0


def headline_tiles(result: Result) -> list[dict]:
    """Return a list of {label, value, unit, fmt, subtle} dicts for HTML cards.

    Pulls from the primary phase (read for 100% read, write for 100% write,
    mixed for everything else).
    """
    phase = result.phases.get(result.primary_phase)
    if phase is None:
        phase = next(iter(result.phases.values()), None)
    tiles: list[dict] = []
    if phase is None:
        return tiles

    throughput = phase.throughput_mib_s_last or phase.throughput_mib_s_first
    iops = phase.iops_last or phase.iops_first
    ops = phase.ops_per_s_last or phase.ops_per_s_first
    lat_avg = phase.io_lat_us.avg if phase.io_lat_us else None
    lat_max = phase.io_lat_us.max if phase.io_lat_us else None

    tiles.append({"label": "Throughput", "value": throughput, "unit": "MiB/s", "fmt": ",.1f"})
    if iops is not None:
        tiles.append({"label": "IOPS", "value": iops, "unit": "", "fmt": ",.0f"})
    if ops is not None:
        tiles.append({"label": "Ops/s", "value": ops, "unit": "", "fmt": ",.0f"})
    tiles.append({"label": "Avg latency", "value": lat_avg, "unit": "µs", "fmt": ",.1f"})
    tiles.append({"label": "Max latency", "value": lat_max, "unit": "µs", "fmt": ",.1f"})
    tiles.append({"label": "CPU", "value": phase.cpu_pct, "unit": "%", "fmt": ".1f"})
    tiles.append(
        {"label": "Errors", "value": phase.errors, "unit": "", "fmt": "d",
         "subtle": phase.errors == 0}
    )
    tiles.append(
        {"label": "Duration", "value": result.duration_s, "unit": "s", "fmt": ".1f",
         "subtle": True}
    )
    return tiles


def throughput_per_phase(results: Iterable[Result]) -> go.Figure:
    """Bar chart: one bar per (result, phase) showing throughput MiB/s."""
    fig = go.Figure()
    phase_order = ["write", "read", "mixed"]
    seen_phases: set[str] = set()
    for res in results:
        for phase_name in phase_order:
            if phase_name not in res.phases:
                continue
            seen_phases.add(phase_name)
        for phase_name, _ in res.phases.items():
            if phase_name not in phase_order:
                seen_phases.add(phase_name)
    ordered_phases = [p for p in phase_order if p in seen_phases] + sorted(
        seen_phases - set(phase_order)
    )

    for i, res in enumerate(results):
        label = f"{res.target.name}·{res.workload.name}"
        ys = []
        for phase_name in ordered_phases:
            phase = res.phases.get(phase_name)
            ys.append(_safe(phase.throughput_mib_s_last if phase else None) or
                      _safe(phase.throughput_mib_s_first if phase else None))
        fig.add_bar(name=label, x=ordered_phases, y=ys,
                    marker_color=PALETTE[i % len(PALETTE)], hovertemplate="%{y:,.1f} MiB/s<extra>%{fullData.name}</extra>")
    _apply_theme(fig, title="Throughput by phase", y_title="MiB/s", x_title="phase")
    fig.update_layout(barmode="group", bargap=0.25)
    return fig


def latency_overview(results: Iterable[Result]) -> go.Figure | None:
    """min / avg / max latency per phase as grouped bars."""
    xs: list[str] = []
    mins: list[float] = []
    avgs: list[float] = []
    maxs: list[float] = []
    for res in results:
        for phase_name, phase in res.phases.items():
            xs.append(phase_name)
            mins.append(_safe(phase.io_lat_us.min))
            avgs.append(_safe(phase.io_lat_us.avg))
            maxs.append(_safe(phase.io_lat_us.max))
    if not xs:
        return None
    fig = go.Figure()
    fig.add_bar(name="min", x=xs, y=mins, marker_color=PALETTE[1],
                hovertemplate="%{y:,.0f} µs<extra>min</extra>")
    fig.add_bar(name="avg", x=xs, y=avgs, marker_color=PALETTE[0],
                hovertemplate="%{y:,.0f} µs<extra>avg</extra>")
    fig.add_bar(name="max", x=xs, y=maxs, marker_color=PALETTE[3],
                hovertemplate="%{y:,.0f} µs<extra>max</extra>")
    _apply_theme(fig, title="IO latency (min / avg / max)", y_title="µs", x_title="phase")
    fig.update_layout(barmode="group", bargap=0.25)
    return fig


# ---------------------------------------------------------------------------
# Sweep overlay (compare report)
# ---------------------------------------------------------------------------


def sweep_overlay(
    *,
    title: str,
    x_labels: list[str],
    x_axis_title: str,
    y_title: str,
    series: list[tuple[str, list[float | None]]],
    hover_unit: str = "",
    initial_mode: str = "bar",
    sort_numeric: bool = False,
) -> go.Figure:
    """Multi-series chart with a live Bar/Line toggle (Plotly updatemenus).

    Both representations (bars + lines) are baked in at render time; the
    toggle flips trace visibility on the client. No JS callbacks needed.

    Args:
      title:         chart title
      x_labels:      one x value per data point (already formatted)
      x_axis_title:  axis label (e.g. "block size")
      y_title:       y-axis label
      series:        list of (run_label, [y values aligned with x_labels])
      hover_unit:    appended to hover values, e.g. "MiB/s"
      initial_mode:  "bar" or "line"
      sort_numeric:  if x_labels look like sized values (k/m/g suffixes or pure
                     ints), keep them ordered as given but display on a
                     numeric axis. Otherwise treat as categorical.
    """
    fig = go.Figure()
    n = len(series)
    # Bar traces first (initially visible).
    for i, (name, ys) in enumerate(series):
        color = PALETTE[i % len(PALETTE)]
        fig.add_bar(
            name=name,
            x=x_labels,
            y=[_safe(v) for v in ys],
            visible=(initial_mode == "bar"),
            marker_color=color,
            marker_line_width=0,
            hovertemplate=(
                "%{x} • %{y:,.1f} " + hover_unit + "<extra>" + name + "</extra>"
            ).strip(),
        )
    # Scatter+line traces second (initially hidden).
    for i, (name, ys) in enumerate(series):
        color = PALETTE[i % len(PALETTE)]
        fig.add_scatter(
            name=name,
            x=x_labels,
            y=[_safe(v) for v in ys],
            mode="lines+markers",
            visible=(initial_mode == "line"),
            line=dict(color=color, width=2.5, shape="spline", smoothing=0.6),
            marker=dict(color=color, size=8, line=dict(width=2, color="#0e1116")),
            hovertemplate=(
                "%{x} • %{y:,.1f} " + hover_unit + "<extra>" + name + "</extra>"
            ).strip(),
            showlegend=False,  # legend already covered by the bar traces
        )

    _apply_theme(fig, title=title, x_title=x_axis_title, y_title=y_title)
    fig.update_layout(
        barmode="group",
        bargap=0.25,
        updatemenus=_toggle_buttons(n_series=n),
    )
    # When the user toggles, the legend needs to follow.
    # Easiest: rebuild visibility + showlegend together via per-button args.
    if fig.layout.updatemenus:
        bar_visible = [True] * n + [False] * n
        line_visible = [False] * n + [True] * n
        bar_showleg = [True] * n + [False] * n
        line_showleg = [False] * n + [True] * n
        fig.layout.updatemenus[0].buttons = [
            dict(
                label="Bar",
                method="update",
                args=[{"visible": bar_visible, "showlegend": bar_showleg}],
            ),
            dict(
                label="Line",
                method="update",
                args=[{"visible": line_visible, "showlegend": line_showleg}],
            ),
        ]
    return fig


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def render_figure_html(fig: go.Figure | None, *, full_html: bool = False) -> str:
    if fig is None:
        return ""
    return fig.to_html(
        include_plotlyjs=False,
        full_html=full_html,
        config={"displaylogo": False, "responsive": True, "displayModeBar": False},
    )
