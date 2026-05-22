"""Plotly figure factories. v0.1 covers the headline tiles + a per-phase throughput bar.

Each factory takes a list[Result] and returns a plotly.graph_objects.Figure or
a list of (title, html_snippet) tuples for embedding in the Jinja template.
"""

from __future__ import annotations

from typing import Iterable

import plotly.graph_objects as go

from ..results.schema import PhaseResult, Result


def _safe(v):
    return v if v is not None else 0


def headline_tiles(result: Result) -> list[dict]:
    """Return a list of {label, value, unit, subtle} dicts for HTML cards.

    Pulls from the primary phase (read for 100% read, write for 100% write,
    mixed for everything else).
    """
    phase = result.phases.get(result.primary_phase)
    if phase is None:
        # Fall back to whatever phase we have.
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
    tiles.append(
        {
            "label": "CPU",
            "value": phase.cpu_pct,
            "unit": "%",
            "fmt": ".1f",
        }
    )
    tiles.append(
        {
            "label": "Errors",
            "value": phase.errors,
            "unit": "",
            "fmt": "d",
            "subtle": phase.errors == 0,
        }
    )
    tiles.append(
        {
            "label": "Duration",
            "value": result.duration_s,
            "unit": "s",
            "fmt": ".1f",
            "subtle": True,
        }
    )
    return tiles


def throughput_per_phase(results: Iterable[Result]) -> go.Figure:
    """Bar chart: one bar per (result, phase) showing throughput MiB/s.

    For v0.1 with a single RunSpec, this shows each elbencho phase (write,
    read) side-by-side so you can see the populate cost vs the read cost.
    """
    fig = go.Figure()
    for res in results:
        label = f"{res.target.name}·{res.workload.name}"
        for phase_name, phase in res.phases.items():
            y = phase.throughput_mib_s_last or phase.throughput_mib_s_first or 0
            fig.add_bar(name=f"{label}·{phase_name}", x=[phase_name], y=[y])
    fig.update_layout(
        title="Throughput by phase",
        yaxis_title="MiB/s",
        xaxis_title="phase",
        barmode="group",
        template="plotly_white",
        height=420,
    )
    return fig


def latency_overview(results: Iterable[Result]) -> go.Figure | None:
    """min / avg / max latency per phase as grouped bars."""
    xs: list[str] = []
    mins: list[float] = []
    avgs: list[float] = []
    maxs: list[float] = []
    for res in results:
        for phase_name, phase in res.phases.items():
            xs.append(f"{res.target.name}·{phase_name}")
            mins.append(_safe(phase.io_lat_us.min))
            avgs.append(_safe(phase.io_lat_us.avg))
            maxs.append(_safe(phase.io_lat_us.max))
    if not xs:
        return None
    fig = go.Figure()
    fig.add_bar(name="min", x=xs, y=mins)
    fig.add_bar(name="avg", x=xs, y=avgs)
    fig.add_bar(name="max", x=xs, y=maxs)
    fig.update_layout(
        title="IO latency (min / avg / max)",
        yaxis_title="µs",
        barmode="group",
        template="plotly_white",
        height=420,
    )
    return fig


def render_figure_html(fig: go.Figure | None, *, full_html: bool = False) -> str:
    if fig is None:
        return ""
    return fig.to_html(
        include_plotlyjs=False,
        full_html=full_html,
        config={"displaylogo": False, "responsive": True},
    )
