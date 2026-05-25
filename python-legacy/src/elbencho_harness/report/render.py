"""Render Result(s) into a self-contained HTML report."""

from __future__ import annotations

from pathlib import Path

from jinja2 import Environment, FileSystemLoader, select_autoescape

from ..results.schema import Result
from .charts import (
    headline_tiles,
    latency_overview,
    render_figure_html,
    throughput_per_phase,
)

TEMPLATES_DIR = Path(__file__).parent / "templates"


def _env() -> Environment:
    env = Environment(
        loader=FileSystemLoader(str(TEMPLATES_DIR)),
        autoescape=select_autoescape(["html", "htm", "xml"]),
        trim_blocks=True,
        lstrip_blocks=True,
    )
    return env


def render_single(result: Result, out_path: Path, *, run_label: str | None = None) -> Path:
    env = _env()
    template = env.get_template("report.html.j2")
    tiles = headline_tiles(result)
    throughput_html = render_figure_html(throughput_per_phase([result]))
    latency_html = render_figure_html(latency_overview([result]))
    html = template.render(
        result=result,
        tiles=tiles,
        run_label=run_label or f"{result.target.name} · {result.workload.name}",
        throughput_html=throughput_html,
        latency_html=latency_html,
    )
    out_path.write_text(html, encoding="utf-8")
    return out_path
