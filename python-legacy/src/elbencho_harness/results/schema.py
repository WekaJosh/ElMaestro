"""Canonical Result schema, versioned. The contract between engine and report layers."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

SCHEMA_VERSION = "1.0"


class _Model(BaseModel):
    model_config = ConfigDict(extra="forbid")


class LatencyBucket(_Model):
    min: float | int | None = None
    avg: float | int | None = None
    max: float | int | None = None


class PhaseResult(_Model):
    operation: str  # 'read' | 'write' | 'mixed' | raw elbencho op tag
    throughput_mib_s_first: float | int | None = None
    throughput_mib_s_last: float | int | None = None
    iops_first: float | int | None = None
    iops_last: float | int | None = None
    ops_per_s_first: float | int | None = None  # S3 mode populates this
    ops_per_s_last: float | int | None = None
    entries: float | int | None = None
    mib_total: float | int | None = None
    cpu_pct: float | int | None = None
    errors: int = 0
    io_lat_us: LatencyBucket = Field(default_factory=LatencyBucket)
    ent_lat_us: LatencyBucket = Field(default_factory=LatencyBucket)
    latency_percentiles_us: dict[str, float | int] = Field(default_factory=dict)
    raw: dict[str, Any] = Field(default_factory=dict)


class ClientInfo(_Model):
    host: str
    elbencho_version: str | None = None
    features: list[str] = Field(default_factory=list)


class EngineArtifactRefs(_Model):
    """Where the engine wrote its output on disk.

    Field names predate fio support; they're kept for backwards compat with
    v1.0 result.json files. Fields that don't apply to the current engine
    can be left None (e.g. fio populates jsonfile_path and stdout_path only).
    """

    command: str
    stdout_path: str
    csv_path: str | None = None
    jsonfile_path: str | None = None
    livecsv_path: str | None = None


# Backwards-compat alias.
ElbenchoArtifactRefs = EngineArtifactRefs


class TargetSnapshot(_Model):
    kind: str
    name: str
    detail: dict[str, Any] = Field(default_factory=dict)


class WorkloadSnapshot(_Model):
    name: str
    block_size: int
    rw_mix_pct_read: int
    threads_per_client: int
    io_depth: int
    pattern: str
    direct_io: bool
    duration_s: int | None = None
    dataset_size: int | None = None
    file_size: int | None = None
    file_count: int | None = None
    total_concurrency: int  # threads_per_client * client_count


class Result(_Model):
    schema_version: str = SCHEMA_VERSION
    run_id: str
    spec_hash: str
    engine: str = "elbencho"  # which backend produced this Result
    primary_phase: str  # which phase is the report headline
    started_at: datetime
    finished_at: datetime
    duration_s: float
    target: TargetSnapshot
    workload: WorkloadSnapshot
    clients: list[ClientInfo]
    # Field name is historical: predates fio support. Holds the engine's
    # artifact refs regardless of which engine produced them.
    elbencho: EngineArtifactRefs
    phases: dict[str, PhaseResult]  # keyed by 'read' | 'write' | 'mixed'
    elbencho_exit_code: int  # historical name; holds the engine's exit code
    errors: list[str] = Field(default_factory=list)
    notes: str = ""
