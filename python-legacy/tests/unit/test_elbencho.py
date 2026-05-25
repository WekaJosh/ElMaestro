"""Tests for the elbencho flag builder + CSV parser."""

from __future__ import annotations

from pathlib import Path

from elbencho_harness.config.models import (
    ClientHost,
    PosixTarget,
    RunSpec,
    S3Target,
    Workload,
)
from elbencho_harness.engine.elbencho import (
    artifacts_for,
    build_argv,
    parse_csv,
)


def _make_spec(target, workload):
    return RunSpec(
        run_id="01",
        spec_hash="sha256:x",
        target=target,
        workload=workload,
        clients=[ClientHost()],
    )


def test_build_argv_read_only_runs_write_then_read(tmp_path):
    spec = _make_spec(
        PosixTarget(name="t", mount_path="/mnt", dataset_subdir="bench"),
        Workload(
            name="w",
            block_size=1048576,
            rw_mix_pct_read=100,
            threads_per_client=8,
            io_depth=4,
            file_size=1048576,
            file_count=4,
            direct_io=True,
        ),
    )
    artifacts = artifacts_for(tmp_path)
    argv, primary = build_argv(spec, artifacts)
    assert primary == "read"
    assert "-w" in argv and "-r" in argv
    assert "--direct" in argv
    assert "-b" in argv and "1048576" in argv
    assert "-t" in argv and "8" in argv
    assert "--iodepth" in argv and "4" in argv
    assert str(Path("/mnt/bench")) in argv


def test_build_argv_write_only(tmp_path):
    spec = _make_spec(
        PosixTarget(name="t", mount_path="/mnt"),
        Workload(name="w", block_size=4096, rw_mix_pct_read=0, file_size=4096, file_count=1),
    )
    argv, primary = build_argv(spec, artifacts_for(tmp_path))
    assert primary == "write"
    assert "-w" in argv
    assert "-r" not in argv


def test_build_argv_mixed_uses_rwmixpct(tmp_path):
    spec = _make_spec(
        PosixTarget(name="t", mount_path="/mnt"),
        Workload(name="w", block_size=4096, rw_mix_pct_read=70, file_size=4096, file_count=1),
    )
    argv, primary = build_argv(spec, artifacts_for(tmp_path))
    assert primary == "mixed"
    assert "--rwmixpct" in argv
    assert "70" in argv


def test_build_argv_s3_target(tmp_path):
    spec = _make_spec(
        S3Target(
            name="s3",
            endpoint="https://s3.example.com",
            bucket="bench-bucket",
            credentials_ref="env:S3_X",
            addressing="virtual",
        ),
        Workload(name="w", block_size=1048576, rw_mix_pct_read=0, file_size=4096, file_count=1),
    )
    argv, _ = build_argv(spec, artifacts_for(tmp_path))
    assert "--s3endpoints" in argv
    assert "https://s3.example.com" in argv
    assert "--s3virtaddr" in argv
    assert "bench-bucket" in argv
    # POSIX-only flags must NOT appear for S3 targets
    assert "--direct" not in argv
    assert "--dropcache" not in argv


def test_parse_csv_extracts_iops_and_throughput(tmp_path):
    csv = tmp_path / "run.csv"
    csv.write_text(
        "operation,IOPS [first],IOPS [last],MiB/s [first],MiB/s [last],"
        "IO lat us min,IO lat us avg,IO lat us max\n"
        "READ,18432,17900,18432.1,17900.0,12,612,7800\n"
        "WRITE,820,790,820.5,790.1,210,1200,8200\n"
    )
    rows = parse_csv(csv)
    assert len(rows) == 2
    read = [r for r in rows if r.operation == "READ"][0]
    write = [r for r in rows if r.operation == "WRITE"][0]
    assert read.metrics["iops_last"] == 17900
    assert read.metrics["mibps_first"] == 18432.1
    assert read.metrics["io_lat_us_avg"] == 612
    assert write.metrics["iops_first"] == 820
