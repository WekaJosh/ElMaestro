"""Event-driven runner: iterate specs, yield progress events.

Decouples the TUI from the engine: this module knows how to drive a sequence
of (SweepPoint, RunSpec) pairs through the coordinator and emit one event per
status change. The TUI worker consumes these events and posts them to the UI
thread.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterator

import ulid

from ..config.loader import load_run_plan
from ..config.models import RunPlan
from ..config.sweep import SweepPoint, materialize_run_refs
from ..engine.coordinator import CoordinatorError, run as run_coord
from ..report.render import render_single
from ..results.schema import Result
from ..results.store import (
    Manifest,
    new_run_dir,
    spec_dir,
    write_manifest,
    write_result,
)


@dataclass
class SpecPlanned:
    index: int
    target: str
    workload: str
    axis_label: str
    spec_hash: str


@dataclass
class SpecStarted:
    index: int
    spec_hash: str
    started_at: datetime


@dataclass
class SpecFinished:
    index: int
    spec_hash: str
    status: str  # 'completed' | 'failed:<rc>' | 'error'
    result: Result | None  # None for coordinator errors
    duration_s: float
    report_path: Path | None


@dataclass
class RunStarted:
    run_dir: Path
    total: int


@dataclass
class RunFinished:
    run_dir: Path
    completed: int
    failed: int


Event = SpecPlanned | SpecStarted | SpecFinished | RunStarted | RunFinished


def plan_events(config: Path) -> tuple[RunPlan, list[tuple[SweepPoint | None, object]]]:
    """Load the config and return (plan, expanded pairs). Pure / synchronous."""
    plan = load_run_plan(config)
    pairs = materialize_run_refs(plan)
    return plan, pairs


def execute(config: Path, *, output_dir: Path | None = None) -> Iterator[Event]:
    """Iterator entrypoint. Yields one event per phase / per spec.

    The TUI calls this from a worker thread; each yielded event becomes a
    Textual Message posted back to the UI.
    """
    plan, pairs = plan_events(config)
    if not pairs:
        return
    base_out = (output_dir or plan.output_dir).resolve()
    base_out.mkdir(parents=True, exist_ok=True)

    first_point, first_spec = pairs[0]
    label = (
        f"sweep_{first_point.sweep_name}" if first_point is not None
        else f"{first_spec.target.name}_{first_spec.workload.name}"
    )
    run_dir = new_run_dir(base_out, label)
    manifest = Manifest(run_id=ulid.new().str, created_at=datetime.now(timezone.utc))

    yield RunStarted(run_dir=run_dir, total=len(pairs))
    for idx, (point, spec) in enumerate(pairs, start=1):
        axis_label = point.short_label() if point else ""
        yield SpecPlanned(
            index=idx,
            target=spec.target.name,
            workload=spec.workload.name,
            axis_label=axis_label,
            spec_hash=spec.spec_hash,
        )

    completed = 0
    failed = 0
    for idx, (point, spec) in enumerate(pairs, start=1):
        sd = spec_dir(
            run_dir, idx, spec.target.name, spec.workload.name,
            label=(point.short_label() if point else None),
        )
        started = datetime.now(timezone.utc)
        yield SpecStarted(index=idx, spec_hash=spec.spec_hash, started_at=started)
        try:
            result: Result | None = run_coord(spec, spec_dir=sd, engine=plan.engine)
        except CoordinatorError:
            result = None
        duration = (datetime.now(timezone.utc) - started).total_seconds()
        if result is None:
            status = "error"
            report_path = None
            failed += 1
        elif result.elbencho_exit_code == 0:
            write_result(sd, result)
            report_path = render_single(result, sd / "report.html")
            status = "completed"
            completed += 1
        else:
            write_result(sd, result)
            report_path = render_single(result, sd / "report.html")
            status = f"failed:{result.elbencho_exit_code}"
            failed += 1
        manifest.statuses[spec.spec_hash] = status
        manifest.run_specs.append(
            {
                "index": idx,
                "spec_hash": spec.spec_hash,
                "run_id": spec.run_id,
                "target": spec.target.name,
                "workload": spec.workload.name,
                "sweep": point.sweep_name if point else None,
                "axis_values": point.overrides if point else None,
                "spec_dir": str(sd.relative_to(run_dir)),
            }
        )
        write_manifest(run_dir, manifest)
        yield SpecFinished(
            index=idx,
            spec_hash=spec.spec_hash,
            status=status,
            result=result,
            duration_s=duration,
            report_path=report_path,
        )
    yield RunFinished(run_dir=run_dir, completed=completed, failed=failed)
