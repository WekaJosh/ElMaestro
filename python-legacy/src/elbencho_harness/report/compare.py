"""Compare report: overlay N run directories into one HTML.

A `bench run` produces a directory of result.json files (one per spec, possibly
many for a sweep). `bench compare A/ B/ C/` aligns those results across runs
and renders a multi-series report.

Alignment rules:
  - Each run is identified by its directory basename (or --label).
  - Specs match if they share workload_name AND target_name. Sweep axis values
    are surfaced as additional context in the table.
  - Baseline = first run argument (or --baseline). Deltas are pct change.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from jinja2 import Environment, FileSystemLoader, select_autoescape

from ..results.schema import PhaseResult, Result
from ..results.store import list_results, read_manifest
from .charts import (
    axis_pretty_name,
    detect_common_axis,
    format_axis_value,
    overlay_with_toggle,
    render_figure_html,
    sweep_overlay,
)
from .render import TEMPLATES_DIR

import plotly.graph_objects as go


@dataclass
class ResultWithAxes:
    """One result paired with the sweep axis-values that produced it.

    Pairing happens by spec_dir, not spec_hash, because two sweep points can
    produce the same effective workload (and hence same spec_hash) when their
    overrides happen to equal the base workload's values.
    """

    result: Result
    axes: dict[str, Any] | None


@dataclass
class LoadedRun:
    """One run directory unrolled into its label + result list."""

    label: str
    run_dir: Path
    results: list[ResultWithAxes]


def load_run(run_dir: Path, *, label: str | None = None) -> LoadedRun:
    """Load a run directory in manifest order, pairing each result with its
    sweep axis-values."""
    rdir = run_dir.resolve()
    paired: list[ResultWithAxes] = []
    try:
        mf = read_manifest(rdir)
    except FileNotFoundError:
        mf = None
    if mf is not None:
        for entry in mf.run_specs:
            sd = rdir / entry["spec_dir"]
            rj = sd / "result.json"
            if not rj.is_file():
                continue
            try:
                result = Result.model_validate_json(rj.read_text())
            except Exception:
                continue
            paired.append(ResultWithAxes(result=result, axes=entry.get("axis_values")))
    else:
        # No manifest, fall back to listing result.json files in directory order.
        for r in list_results(rdir):
            paired.append(ResultWithAxes(result=r, axes=None))
    return LoadedRun(label=label or rdir.name, run_dir=rdir, results=paired)


def _spec_key(rwa: ResultWithAxes) -> tuple:
    """Stable identifier for matching specs across runs.

    Uses (target_name, workload_name, axis_values). Two specs with the same
    target+workload but different axis values won't collapse.
    """
    axis_tuple = tuple(sorted((rwa.axes or {}).items()))
    return (rwa.result.target.name, rwa.result.workload.name, axis_tuple)


def _primary_phase(r: Result) -> PhaseResult | None:
    return r.phases.get(r.primary_phase) or next(iter(r.phases.values()), None)


def _throughput_of(r: Result) -> float | None:
    p = _primary_phase(r)
    if p is None:
        return None
    v = p.throughput_mib_s_last or p.throughput_mib_s_first
    return float(v) if v is not None else None


def _iops_of(r: Result) -> float | None:
    p = _primary_phase(r)
    if p is None:
        return None
    v = p.iops_last or p.iops_first
    return float(v) if v is not None else None


def _avg_lat_of(r: Result) -> float | None:
    p = _primary_phase(r)
    if p is None or p.io_lat_us is None:
        return None
    return float(p.io_lat_us.avg) if p.io_lat_us.avg is not None else None


@dataclass
class CompareRow:
    spec_key: tuple
    target: str
    workload: str
    axis_label: str          # composite human label, e.g. "block_size=64KiB"
    axes: dict[str, Any] | None  # raw axis-values dict from the manifest
    per_run: dict[str, dict[str, float | None]]  # run_label -> {metric: value}

    def baseline_value(self, metric: str, baseline_label: str) -> float | None:
        return self.per_run.get(baseline_label, {}).get(metric)

    def delta_pct(self, metric: str, run_label: str, baseline_label: str) -> float | None:
        base = self.baseline_value(metric, baseline_label)
        v = self.per_run.get(run_label, {}).get(metric)
        if base in (None, 0) or v is None:
            return None
        return (v - base) / base * 100.0


def build_rows(runs: list[LoadedRun]) -> list[CompareRow]:
    """Align results across runs by spec_key. Returns one row per distinct spec."""
    keyed: dict[tuple, CompareRow] = {}
    for run in runs:
        for rwa in run.results:
            r = rwa.result
            key = _spec_key(rwa)
            row = keyed.get(key)
            if row is None:
                row = CompareRow(
                    spec_key=key,
                    target=r.target.name,
                    workload=r.workload.name,
                    axis_label=_format_axes(rwa.axes),
                    axes=rwa.axes,
                    per_run={},
                )
                keyed[key] = row
            row.per_run[run.label] = {
                "throughput_mib_s": _throughput_of(r),
                "iops": _iops_of(r),
                "avg_lat_us": _avg_lat_of(r),
                "duration_s": r.duration_s,
            }
    # Stable order: target, workload, then by the *numeric* axis value if one is
    # shared (so the chart x-axis sorts naturally for sweeps), else by label.
    common_axis = detect_common_axis([r.axes for r in keyed.values()])
    if common_axis is not None:
        return sorted(
            keyed.values(),
            key=lambda r: (r.target, r.workload, _numeric_axis_value(r.axes, common_axis)),
        )
    return sorted(
        keyed.values(), key=lambda r: (r.target, r.workload, r.axis_label)
    )


def _numeric_axis_value(axes: dict[str, Any] | None, axis: str) -> float:
    """Pull the sweep axis value as a number for sorting. Strings sort last."""
    if not axes:
        return float("inf")
    v = axes.get(axis)
    try:
        return float(v)
    except (TypeError, ValueError):
        return float("inf")


def _format_axes(axes: dict[str, Any] | None) -> str:
    if not axes:
        return ""
    parts = []
    for k, v in sorted(axes.items()):
        parts.append(f"{k}={v}")
    return ", ".join(parts)


# --- charts -----------------------------------------------------------------


def _chart_axis(rows: list[CompareRow]) -> tuple[list[str], str]:
    """Return (x_labels, x_axis_title) for a chart.

    When every row's axes dict has the same single axis (a clean 1-D sweep),
    use the axis value alone as the tick label ("64k", "4m", "16"). Otherwise
    fall back to the composite per-row label ("target·workload·axes").
    """
    common = detect_common_axis([r.axes for r in rows])
    if common is not None:
        x_labels = [format_axis_value(common, r.axes[common]) for r in rows]
        return x_labels, axis_pretty_name(common)
    # Multi-axis fallback: compose target + axes if any.
    fallback: list[str] = []
    for r in rows:
        s = f"{r.target}·{r.workload}"
        if r.axis_label:
            s += f"·{r.axis_label}"
        fallback.append(s)
    return fallback, ""


def _series_from_rows(
    runs: list[LoadedRun], rows: list[CompareRow], metric: str
) -> list[tuple[str, list[float | None]]]:
    return [
        (run.label, [r.per_run.get(run.label, {}).get(metric) for r in rows])
        for run in runs
    ]


def throughput_overlay(runs: list[LoadedRun]) -> go.Figure:
    """Throughput overlay with live Bar/Line toggle."""
    rows = build_rows(runs)
    x_labels, x_title = _chart_axis(rows)
    return sweep_overlay(
        title="Throughput",
        x_labels=x_labels,
        x_axis_title=x_title,
        y_title="MiB/s",
        series=_series_from_rows(runs, rows, "throughput_mib_s"),
        hover_unit="MiB/s",
    )


def iops_overlay(runs: list[LoadedRun]) -> go.Figure:
    """IOPS overlay with live Bar/Line toggle."""
    rows = build_rows(runs)
    x_labels, x_title = _chart_axis(rows)
    return sweep_overlay(
        title="IOPS",
        x_labels=x_labels,
        x_axis_title=x_title,
        y_title="IOPS",
        series=_series_from_rows(runs, rows, "iops"),
        hover_unit="IOPS",
    )


def latency_overlay(runs: list[LoadedRun]) -> go.Figure:
    """Average IO latency overlay with live Bar/Line toggle."""
    rows = build_rows(runs)
    x_labels, x_title = _chart_axis(rows)
    return sweep_overlay(
        title="Average IO latency",
        x_labels=x_labels,
        x_axis_title=x_title,
        y_title="µs",
        series=_series_from_rows(runs, rows, "avg_lat_us"),
        hover_unit="µs",
    )


# --- rendering --------------------------------------------------------------


def render_compare(
    runs: list[LoadedRun],
    out_path: Path,
    *,
    baseline_label: str | None = None,
    title: str = "elbencho-harness compare",
) -> Path:
    """Write a self-contained HTML compare report. Returns the path written."""
    if not runs:
        raise ValueError("render_compare requires at least one LoadedRun")
    baseline = baseline_label or runs[0].label
    rows = build_rows(runs)
    env = Environment(
        loader=FileSystemLoader(str(TEMPLATES_DIR)),
        autoescape=select_autoescape(["html", "htm", "xml"]),
        trim_blocks=True,
        lstrip_blocks=True,
    )
    template = env.get_template("compare.html.j2")
    n_series = len(runs)
    html = template.render(
        title=title,
        runs=runs,
        rows=rows,
        baseline=baseline,
        throughput_html=overlay_with_toggle(throughput_overlay(runs), n_series=n_series),
        iops_html=overlay_with_toggle(iops_overlay(runs), n_series=n_series),
        latency_html=overlay_with_toggle(latency_overlay(runs), n_series=n_series),
    )
    out_path.write_text(html, encoding="utf-8")
    return out_path
