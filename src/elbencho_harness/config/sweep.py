"""Sweep -> list[RunSpec] expansion.

A Sweep names a base workload, a set of target(s), and one or more axes whose
values to vary. Two expansion orders:

  - cartesian: full cross-product of all populated axes.
  - ladder:    vary one axis at a time, holding the rest at the base workload's
               values. Useful when the cartesian count blows up but you want
               to keep each axis represented.

The axis name `client_count` is special: it doesn't override a workload field
but instead trims the client list to the first N entries. Useful for "scale
test" sweeps that show how throughput grows with each added client.
"""

from __future__ import annotations

from dataclasses import dataclass
from itertools import product
from typing import Any, Iterable

import ulid

from .models import ClientHost, RunPlan, RunSpec, Sweep, Workload

# Axes that map directly onto a Workload field. Order matters: expansion uses
# this sequence so two invocations against the same plan produce identical
# point ordering (and hence identical spec directory names on resume).
_WORKLOAD_AXIS_FIELDS: tuple[str, ...] = (
    "block_size",
    "rw_mix_pct_read",
    "threads_per_client",
    "io_depth",
    "dataset_size",
)
# Axes that don't live on Workload.
_SPECIAL_AXES: tuple[str, ...] = ("client_count",)


@dataclass(frozen=True)
class SweepPoint:
    """One concrete combination of axis values, before becoming a RunSpec.

    Carries the sweep name + the dict of overrides applied. Useful for naming
    spec directories and for the report layer to know what each spec varied.
    """

    sweep_name: str
    target_name: str
    overrides: dict[str, Any]

    def short_label(self) -> str:
        """e.g. 'bs=1MiB_t=4_qd=8' — for spec dir names and report axes."""
        parts: list[str] = []
        for k, v in self.overrides.items():
            parts.append(f"{_short_key(k)}={_short_val(k, v)}")
        return "_".join(parts) if parts else "base"


def _short_key(k: str) -> str:
    return {
        "block_size": "bs",
        "rw_mix_pct_read": "rwread",
        "threads_per_client": "t",
        "io_depth": "qd",
        "dataset_size": "ds",
        "client_count": "n",
    }.get(k, k)


def _short_val(k: str, v: Any) -> str:
    if k in {"block_size", "dataset_size"}:
        return _human_bytes(int(v))
    return str(v)


def _human_bytes(n: int) -> str:
    for unit, base in (("GiB", 1024**3), ("MiB", 1024**2), ("KiB", 1024)):
        if n % base == 0 and n >= base:
            return f"{n // base}{unit}"
    return f"{n}B"


def _axis_iter(axes_model) -> dict[str, list]:
    """Pull populated axes off the SweepAxis model as a name->list dict."""
    out: dict[str, list] = {}
    if axes_model is None:
        return out
    for name in (*_WORKLOAD_AXIS_FIELDS, *_SPECIAL_AXES):
        val = getattr(axes_model, name, None)
        if val:
            out[name] = list(val)
    return out


def _apply_overrides_to_workload(base: Workload, overrides: dict[str, Any]) -> Workload:
    """Return a clone of base with the relevant overrides applied."""
    field_set = set(_WORKLOAD_AXIS_FIELDS)
    workload_changes = {k: v for k, v in overrides.items() if k in field_set}
    if not workload_changes:
        return base
    return base.model_copy(update=workload_changes)


def _apply_client_count(clients: list[ClientHost], overrides: dict[str, Any]) -> list[ClientHost]:
    n = overrides.get("client_count")
    if n is None:
        return clients
    if n > len(clients):
        raise ValueError(
            f"sweep axis client_count={n} exceeds available clients ({len(clients)})"
        )
    return clients[:n]


def _expand_cartesian(axes: dict[str, list]) -> Iterable[dict[str, Any]]:
    """Full cross-product of all populated axes."""
    if not axes:
        yield {}
        return
    names = list(axes.keys())
    for combo in product(*(axes[n] for n in names)):
        yield dict(zip(names, combo))


def _expand_ladder(axes: dict[str, list]) -> Iterable[dict[str, Any]]:
    """Vary one axis at a time. The baseline (all axes unchanged) appears once.

    For axes={'block_size':[A,B,C], 'threads_per_client':[1,2,3]}, ladder
    produces: {bs=A}, {bs=B}, {bs=C}, {t=1}, {t=2}, {t=3}. That's 6 points;
    cartesian would be 9. Deduplication is NOT performed across axes that
    happen to share values.
    """
    if not axes:
        yield {}
        return
    for name, values in axes.items():
        for v in values:
            yield {name: v}


def expand(plan: RunPlan, sweep: Sweep) -> list[SweepPoint]:
    """Expand one Sweep against its RunPlan. Returns the ordered list of points.

    Order honors sweep.order and respects sweep.max_runs.
    """
    if sweep.base not in {w.name for w in plan.workloads}:
        raise KeyError(f"sweep {sweep.name!r} references unknown base workload: {sweep.base}")

    sweep_targets = sweep.targets or ([sweep.target] if sweep.target else [])
    if not sweep_targets:
        raise ValueError(f"sweep {sweep.name!r} has no targets")

    axes = _axis_iter(sweep.axes)
    expander = _expand_cartesian if sweep.order == "cartesian" else _expand_ladder
    points: list[SweepPoint] = []
    for tname in sweep_targets:
        for combo in expander(axes):
            points.append(
                SweepPoint(sweep_name=sweep.name, target_name=tname, overrides=combo)
            )
            if sweep.max_runs and len(points) >= sweep.max_runs:
                return points
    return points


def expand_all(plan: RunPlan) -> list[SweepPoint]:
    """Expand every sweep in a plan, concatenated in declaration order."""
    out: list[SweepPoint] = []
    for sw in plan.sweeps:
        out.extend(expand(plan, sw))
    return out


def materialize(plan: RunPlan, point: SweepPoint) -> RunSpec:
    """Turn a SweepPoint into a concrete RunSpec by cloning the base workload."""
    target = plan.target_by_name(point.target_name)
    base_wl = plan.workload_by_name(_base_workload_name(plan, point.sweep_name))
    workload = _apply_overrides_to_workload(base_wl, point.overrides)
    clients = _apply_client_count(plan.clients or [ClientHost()], point.overrides)
    return RunSpec(
        run_id=ulid.new().str,
        spec_hash=RunSpec.make_spec_hash(target, workload, clients),
        target=target,
        workload=workload,
        clients=clients,
    )


def _base_workload_name(plan: RunPlan, sweep_name: str) -> str:
    for sw in plan.sweeps:
        if sw.name == sweep_name:
            return sw.base
    raise KeyError(f"sweep not found: {sweep_name}")


def materialize_all(plan: RunPlan) -> list[tuple[SweepPoint, RunSpec]]:
    """One-shot: expand every sweep, materialize every point. Returns ordered pairs."""
    out: list[tuple[SweepPoint, RunSpec]] = []
    for sw in plan.sweeps:
        for point in expand(plan, sw):
            out.append((point, materialize(plan, point)))
    return out


# ---------------------------------------------------------------------------
# Backing reference for the materialize_targets helper to also build RunSpecs
# from plain top-level `runs:` entries. Keeps CLI logic simple.
# ---------------------------------------------------------------------------


def materialize_run_refs(plan: RunPlan) -> list[tuple[SweepPoint | None, RunSpec]]:
    """Combine `runs:` entries and expanded sweeps into one ordered list.

    Plain `runs:` entries come first, then sweeps in declaration order. The
    SweepPoint is None for plain runs.
    """
    out: list[tuple[SweepPoint | None, RunSpec]] = []
    clients: list[ClientHost] = list(plan.clients or [ClientHost()])
    for ref in plan.runs:
        target = plan.target_by_name(ref.target)
        workload = plan.workload_by_name(ref.workload)
        spec = RunSpec(
            run_id=ulid.new().str,
            spec_hash=RunSpec.make_spec_hash(target, workload, clients),
            target=target,
            workload=workload,
            clients=clients,
        )
        out.append((None, spec))
    for sw in plan.sweeps:
        for point in expand(plan, sw):
            out.append((point, materialize(plan, point)))
    return out
