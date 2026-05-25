"""Tests for config/sweep.py: cartesian, ladder, max_runs, client_count."""

from __future__ import annotations

import pytest

from elbencho_harness.config.models import (
    ClientHost,
    PosixTarget,
    RunPlan,
    Sweep,
    SweepAxis,
    Workload,
)
from elbencho_harness.config.sweep import (
    SweepPoint,
    expand,
    materialize,
    materialize_all,
    materialize_run_refs,
)


def _plan(*, axes: SweepAxis | None = None, order: str = "cartesian", max_runs: int | None = None,
          extra_clients: int = 0) -> RunPlan:
    clients = [ClientHost(host="localhost")]
    for i in range(extra_clients):
        clients.append(ClientHost(host=f"h{i + 1}", ssh_user="u"))
    return RunPlan(
        version=1,
        clients=clients,
        targets=[PosixTarget(name="t", mount_path="/mnt", dataset_subdir="bench")],
        workloads=[
            Workload(
                name="base",
                block_size=4096,
                rw_mix_pct_read=100,
                threads_per_client=2,
                io_depth=1,
                file_size=4096,
                file_count=1,
            )
        ],
        sweeps=[
            Sweep(
                name="sw",
                base="base",
                target="t",
                axes=axes or SweepAxis(),
                order=order,
                max_runs=max_runs,
            )
        ],
    )


def test_cartesian_full_product():
    plan = _plan(
        axes=SweepAxis(block_size=[4096, 1048576], threads_per_client=[2, 4, 8]),
        order="cartesian",
    )
    points = expand(plan, plan.sweeps[0])
    assert len(points) == 6  # 2 * 3
    keys = {tuple(sorted(p.overrides.items())) for p in points}
    assert (("block_size", 4096), ("threads_per_client", 2)) in keys
    assert (("block_size", 1048576), ("threads_per_client", 8)) in keys


def test_ladder_one_axis_at_a_time():
    plan = _plan(
        axes=SweepAxis(block_size=[4096, 1048576], threads_per_client=[2, 4, 8]),
        order="ladder",
    )
    points = expand(plan, plan.sweeps[0])
    # 2 block sizes + 3 thread counts = 5 ladder points.
    assert len(points) == 5
    by_axis = {tuple(p.overrides.keys()): 0 for p in points}
    for p in points:
        by_axis[tuple(p.overrides.keys())] += 1
    assert by_axis[("block_size",)] == 2
    assert by_axis[("threads_per_client",)] == 3


def test_max_runs_caps_expansion():
    plan = _plan(
        axes=SweepAxis(block_size=[4096, 1048576], threads_per_client=[2, 4, 8]),
        order="cartesian",
        max_runs=4,
    )
    points = expand(plan, plan.sweeps[0])
    assert len(points) == 4


def test_short_label_humanizes_bytes():
    p = SweepPoint(
        sweep_name="sw",
        target_name="t",
        overrides={"block_size": 1048576, "threads_per_client": 4},
    )
    label = p.short_label()
    assert "bs=1MiB" in label
    assert "t=4" in label


def test_short_label_empty_for_baseline():
    p = SweepPoint(sweep_name="sw", target_name="t", overrides={})
    assert p.short_label() == "base"


def test_materialize_applies_overrides_to_workload():
    plan = _plan(axes=SweepAxis(block_size=[1048576]), order="cartesian")
    point = expand(plan, plan.sweeps[0])[0]
    spec = materialize(plan, point)
    assert spec.workload.block_size == 1048576
    # Other workload fields preserved from base
    assert spec.workload.threads_per_client == 2


def test_materialize_client_count_trims_client_list():
    plan = _plan(
        axes=SweepAxis(client_count=[1, 2]),
        order="cartesian",
        extra_clients=2,  # gives total 3 clients including localhost
    )
    points = expand(plan, plan.sweeps[0])
    specs = [materialize(plan, p) for p in points]
    assert [len(s.clients) for s in specs] == [1, 2]


def test_materialize_client_count_rejects_too_many():
    plan = _plan(axes=SweepAxis(client_count=[5]), order="cartesian", extra_clients=1)
    points = expand(plan, plan.sweeps[0])
    with pytest.raises(ValueError, match="exceeds available clients"):
        materialize(plan, points[0])


def test_materialize_run_refs_combines_runs_and_sweeps():
    """Plain `runs:` entries come first, then sweeps in declaration order."""
    plan = _plan(axes=SweepAxis(block_size=[4096, 8192]), order="cartesian")
    # Add a plain run reference too.
    from elbencho_harness.config.models import RunRef

    plan = plan.model_copy(update={"runs": [RunRef(target="t", workload="base")]})
    pairs = materialize_run_refs(plan)
    # 1 plain run + 2 sweep points
    assert len(pairs) == 3
    assert pairs[0][0] is None  # plain run has no SweepPoint
    assert pairs[1][0] is not None
    assert pairs[2][0] is not None


def test_materialize_all_expands_every_sweep_in_plan():
    plan = _plan(axes=SweepAxis(io_depth=[1, 2, 4]), order="cartesian")
    pairs = materialize_all(plan)
    assert len(pairs) == 3
    io_depths = [s.workload.io_depth for _, s in pairs]
    assert io_depths == [1, 2, 4]


def test_expand_rejects_missing_workload():
    plan = _plan()
    bad = Sweep(name="bad", base="nope", target="t")
    with pytest.raises(KeyError, match="unknown base workload"):
        expand(plan, bad)


def test_expand_baseline_with_no_axes_is_single_point():
    """A sweep with no populated axes should still produce one point (baseline)."""
    plan = _plan()
    points = expand(plan, plan.sweeps[0])
    assert len(points) == 1
    assert points[0].overrides == {}


def test_ladder_axis_order_is_deterministic():
    """Regression: axes were keyed on a set, leading to PYTHONHASHSEED-dependent order.

    A resume run that emits specs in a different order than the original run
    drifts the spec directory layout and ends up writing parallel results dirs
    instead of skipping.
    """
    plan = _plan(
        axes=SweepAxis(
            threads_per_client=[2, 4],
            block_size=[4096, 8192],
            io_depth=[1, 2],
        ),
        order="ladder",
    )
    first = [p.overrides for p in expand(plan, plan.sweeps[0])]
    second = [p.overrides for p in expand(plan, plan.sweeps[0])]
    assert first == second
    # The declared canonical order is block_size -> threads_per_client -> io_depth
    axis_seen_in_order = [next(iter(p)) for p in first]
    # Each axis's first occurrence in the emission must follow that canonical order.
    first_occurrence = {}
    for axis in axis_seen_in_order:
        first_occurrence.setdefault(axis, len(first_occurrence))
    assert list(first_occurrence) == ["block_size", "threads_per_client", "io_depth"]
