"""Smoke tests for config models — focused on byte parsing, validation, and refs."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from elbencho_harness.config.loader import load_run_plan
from elbencho_harness.config.models import (
    PosixTarget,
    RunSpec,
    S3Target,
    Workload,
)


def test_byte_size_parsing():
    w = Workload(
        name="w1",
        block_size="1MiB",  # type: ignore[arg-type]
        file_size="256MiB",  # type: ignore[arg-type]
        file_count=4,
    )
    assert w.block_size == 1024 * 1024
    assert w.file_size == 256 * 1024 * 1024


def test_workload_requires_duration_or_dataset():
    with pytest.raises(ValidationError):
        Workload(name="w", block_size=4096)


def test_s3_credentials_must_be_referenced():
    with pytest.raises(ValidationError):
        S3Target(
            name="s3",
            endpoint="https://s3.example.com",
            bucket="b",
            credentials_ref="ACCESS:SECRET",  # inline secret -> rejected
        )
    # env/file refs accepted
    S3Target(
        name="s3",
        endpoint="https://s3.example.com",
        bucket="b",
        credentials_ref="env:S3_X",
    )


def test_posix_subdir_no_traversal():
    with pytest.raises(ValidationError):
        PosixTarget(name="p", mount_path="/mnt", dataset_subdir="../escape")
    PosixTarget(name="p", mount_path="/mnt", dataset_subdir="sub/dir")


def test_spec_hash_is_deterministic():
    t = PosixTarget(name="p", mount_path="/mnt")
    w = Workload(name="w", block_size=1048576, file_size=1048576, file_count=1)
    h1 = RunSpec.make_spec_hash(t, w, [])
    h2 = RunSpec.make_spec_hash(t, w, [])
    assert h1 == h2
    assert h1.startswith("sha256:")


def test_run_plan_validates_refs(tmp_path):
    cfg = tmp_path / "p.yaml"
    cfg.write_text(
        """
version: 1
targets:
  - {name: t1, kind: posix, mount_path: /tmp}
workloads:
  - {name: w1, block_size: 4KiB, file_size: 1MiB, file_count: 1}
runs:
  - {target: t1, workload: w1}
"""
    )
    plan = load_run_plan(cfg)
    assert plan.target_by_name("t1").name == "t1"
    assert plan.workload_by_name("w1").block_size == 4096


def test_run_plan_rejects_dangling_ref(tmp_path):
    cfg = tmp_path / "p.yaml"
    cfg.write_text(
        """
version: 1
targets:
  - {name: t1, kind: posix, mount_path: /tmp}
workloads:
  - {name: w1, block_size: 4KiB, file_size: 1MiB, file_count: 1}
runs:
  - {target: not-a-target, workload: w1}
"""
    )
    with pytest.raises(ValidationError):
        load_run_plan(cfg)
