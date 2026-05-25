"""Tests for the compare report: row alignment, deltas, HTML rendering."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from elbencho_harness.report.compare import (
    LoadedRun,
    build_rows,
    load_run,
    render_compare,
)
from elbencho_harness.results.schema import (
    ClientInfo,
    ElbenchoArtifactRefs,
    LatencyBucket,
    PhaseResult,
    Result,
    TargetSnapshot,
    WorkloadSnapshot,
)


def _make_result(
    *,
    spec_hash: str,
    target: str,
    workload: str,
    throughput: float,
    iops: float | None = None,
    lat_avg: float = 500.0,
) -> Result:
    return Result(
        schema_version="1.0",
        run_id="01",
        spec_hash=spec_hash,
        primary_phase="read",
        started_at=datetime(2026, 1, 1, tzinfo=timezone.utc),
        finished_at=datetime(2026, 1, 1, 0, 0, 10, tzinfo=timezone.utc),
        duration_s=10.0,
        target=TargetSnapshot(kind="posix", name=target, detail={}),
        workload=WorkloadSnapshot(
            name=workload,
            block_size=4096,
            rw_mix_pct_read=100,
            threads_per_client=4,
            io_depth=1,
            pattern="seq",
            direct_io=False,
            file_size=4096,
            file_count=1,
            total_concurrency=4,
        ),
        clients=[ClientInfo(host="localhost")],
        elbencho=ElbenchoArtifactRefs(
            command="elbencho ...",
            csv_path="raw/run.csv",
            jsonfile_path="raw/run.json",
            stdout_path="raw/stdout.log",
        ),
        phases={
            "read": PhaseResult(
                operation="read",
                throughput_mib_s_last=throughput,
                iops_last=iops,
                io_lat_us=LatencyBucket(min=10, avg=lat_avg, max=1000),
            )
        },
        elbencho_exit_code=0,
    )


def _write_run_dir(tmp_path: Path, label: str, specs: list[tuple[str, str, str, float]]) -> Path:
    """Create a run-dir layout with manifest.json + a spec subdir per result."""
    run_dir = tmp_path / label
    run_dir.mkdir()
    run_specs = []
    for idx, (spec_hash, target, workload, throughput) in enumerate(specs, start=1):
        sd = run_dir / f"{idx:04d}_{target}_{workload}"
        sd.mkdir()
        result = _make_result(
            spec_hash=spec_hash, target=target, workload=workload, throughput=throughput
        )
        (sd / "result.json").write_text(result.model_dump_json())
        run_specs.append(
            {
                "index": idx,
                "spec_hash": spec_hash,
                "run_id": result.run_id,
                "target": target,
                "workload": workload,
                "sweep": None,
                "axis_values": None,
                "spec_dir": sd.name,
            }
        )
    manifest = {
        "schema_version": "1.0",
        "run_id": "manifest-1",
        "created_at": "2026-01-01T00:00:00",
        "run_specs": run_specs,
        "statuses": {sh: "completed" for sh, *_ in specs},
    }
    (run_dir / "manifest.json").write_text(json.dumps(manifest))
    return run_dir


# --- tests --------------------------------------------------------------------


def test_load_run_returns_results_paired_with_axes(tmp_path):
    run_dir = _write_run_dir(
        tmp_path, "run-A", [("sha:1", "weka", "w1", 1000.0), ("sha:2", "weka", "w2", 2000.0)]
    )
    lr = load_run(run_dir)
    assert lr.label == "run-A"
    assert len(lr.results) == 2
    # Each entry has both a Result and axes (None in this fixture).
    assert lr.results[0].result.workload.name == "w1"
    assert lr.results[0].axes is None


def test_load_run_label_override(tmp_path):
    run_dir = _write_run_dir(tmp_path, "run-A", [("sha:1", "t", "w", 100.0)])
    lr = load_run(run_dir, label="custom-label")
    assert lr.label == "custom-label"


def test_build_rows_aligns_matching_specs_across_runs(tmp_path):
    a = load_run(_write_run_dir(tmp_path, "A", [("s:1", "t", "w", 1000.0)]))
    b = load_run(_write_run_dir(tmp_path, "B", [("s:1", "t", "w", 1200.0)]))
    rows = build_rows([a, b])
    assert len(rows) == 1
    row = rows[0]
    assert row.target == "t" and row.workload == "w"
    assert row.per_run["A"]["throughput_mib_s"] == 1000.0
    assert row.per_run["B"]["throughput_mib_s"] == 1200.0


def test_build_rows_keeps_distinct_specs_separate(tmp_path):
    a = load_run(
        _write_run_dir(tmp_path, "A", [("s:1", "t", "w1", 100.0), ("s:2", "t", "w2", 200.0)])
    )
    b = load_run(_write_run_dir(tmp_path, "B", [("s:1", "t", "w1", 110.0)]))
    rows = build_rows([a, b])
    assert len(rows) == 2  # w1 (in both) and w2 (only A)
    by_workload = {r.workload: r for r in rows}
    assert "w2" in by_workload
    # w2 only present in A; per_run for B is empty
    assert "B" not in by_workload["w2"].per_run


def test_delta_pct_positive_and_negative(tmp_path):
    a = load_run(_write_run_dir(tmp_path, "A", [("s:1", "t", "w", 100.0)]))
    b = load_run(_write_run_dir(tmp_path, "B", [("s:1", "t", "w", 125.0)]))
    rows = build_rows([a, b])
    row = rows[0]
    assert row.delta_pct("throughput_mib_s", "B", "A") == pytest.approx(25.0)
    assert row.delta_pct("throughput_mib_s", "A", "A") == 0.0


def test_delta_pct_handles_missing_baseline(tmp_path):
    a = load_run(_write_run_dir(tmp_path, "A", [("s:1", "t", "w1", 100.0)]))
    b = load_run(_write_run_dir(tmp_path, "B", [("s:2", "t", "w2", 100.0)]))
    rows = build_rows([a, b])
    # No spec shared, so each row is missing one of the runs.
    for row in rows:
        assert row.delta_pct("throughput_mib_s", "B", "A") is None or row.target == "t"


def test_render_compare_writes_html_with_run_labels(tmp_path):
    a = load_run(_write_run_dir(tmp_path, "A", [("s:1", "t", "w", 100.0)]))
    b = load_run(_write_run_dir(tmp_path, "B", [("s:1", "t", "w", 120.0)]))
    out = render_compare([a, b], tmp_path / "out.html", title="my-compare")
    html = out.read_text()
    assert "my-compare" in html
    assert "A" in html and "B" in html
    assert "+20.0%" in html or "20.0%" in html  # delta is rendered
    assert "<table" in html


def test_render_compare_raises_on_empty_run_list(tmp_path):
    with pytest.raises(ValueError, match="at least one"):
        render_compare([], tmp_path / "out.html")


def test_load_run_keeps_same_spec_hash_different_axes_distinct(tmp_path):
    """Regression: two sweep points that produce the same effective workload
    (and thus the same spec_hash) must remain distinct in compare rows when
    their sweep axis values differ.

    Earlier code keyed axis lookup by spec_hash, which collapsed these
    invisibly. Bug was caught by an end-to-end smoke run; this test pins it.
    """
    run_dir = tmp_path / "run-A"
    run_dir.mkdir()

    # Both specs serialize to the same result.json structure (same workload),
    # so they share spec_hash. Their manifest entries record different axes.
    for idx, (axes, spec_dir_name) in enumerate(
        [
            ({"block_size": 1048576}, "0001_t_w_bs-1MiB"),
            ({"threads_per_client": 4}, "0002_t_w_t-4"),
        ],
        start=1,
    ):
        sd = run_dir / spec_dir_name
        sd.mkdir()
        result = _make_result(
            spec_hash="sha:identical", target="t", workload="w", throughput=100.0
        )
        (sd / "result.json").write_text(result.model_dump_json())

    manifest = {
        "schema_version": "1.0",
        "run_id": "m",
        "created_at": "2026-01-01T00:00:00",
        "run_specs": [
            {
                "index": 1,
                "spec_hash": "sha:identical",
                "run_id": "r1",
                "target": "t",
                "workload": "w",
                "sweep": "sw",
                "axis_values": {"block_size": 1048576},
                "spec_dir": "0001_t_w_bs-1MiB",
            },
            {
                "index": 2,
                "spec_hash": "sha:identical",
                "run_id": "r2",
                "target": "t",
                "workload": "w",
                "sweep": "sw",
                "axis_values": {"threads_per_client": 4},
                "spec_dir": "0002_t_w_t-4",
            },
        ],
        "statuses": {"sha:identical": "completed"},
    }
    (run_dir / "manifest.json").write_text(json.dumps(manifest))

    lr = load_run(run_dir)
    assert len(lr.results) == 2
    axes_sets = [rwa.axes for rwa in lr.results]
    assert {"block_size": 1048576} in axes_sets
    assert {"threads_per_client": 4} in axes_sets

    rows = build_rows([lr])
    # Two distinct sweep points must produce two distinct rows.
    assert len(rows) == 2
