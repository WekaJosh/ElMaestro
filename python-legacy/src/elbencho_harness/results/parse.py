"""Assemble a canonical Result, plus elbencho-specific CSV parsing helpers.

`build_result` is engine-agnostic: it takes a phases dict (produced by the
backend) plus run metadata and packages everything into a Result. Engine
backends own the parsing of their own native output formats.

The helpers _phase_from_row / _classify_phase / NON_IO_OPERATIONS are kept
here because they're shared between the elbencho backend and the legacy
import path; promoting them to public names is on the v0.8 cleanup list.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import Path
from typing import Any

from ..config.models import PosixTarget, RunSpec, S3Target
from ..engine.elbencho import PhaseRow
from .schema import (
    ClientInfo,
    EngineArtifactRefs,
    LatencyBucket,
    PhaseResult,
    Result,
    TargetSnapshot,
    WorkloadSnapshot,
)


# Operations engines emit as phase rows but which aren't actual IO
# (no MiB/s or IOPS, just directory / sync / cache prep work). They show up
# in raw/ for completeness, but are excluded from the Result.phases dict so
# reports don't render null tiles.
NON_IO_OPERATIONS = {"mkdirs", "rmdirs", "sync", "drop_caches", "cleanup"}


def _classify_phase(operation: str) -> str:
    op = operation.lower()
    if "rwmix" in op or ("mix" in op and "mixed" not in op):
        return "mixed"
    if "read" in op:
        return "read"
    if "write" in op:
        return "write"
    if "mkdir" in op:
        return "mkdirs"
    if "rmdir" in op:
        return "rmdirs"
    if "drop" in op and "cache" in op:
        return "drop_caches"
    if "cleanup" in op:
        return "cleanup"
    if "sync" in op:
        return "sync"
    if "stat" in op:
        return "stat"
    if "del" in op:
        return "delete"
    return operation or "unknown"


def _phase_from_row(row: PhaseRow, is_s3: bool) -> PhaseResult:
    """Translate one elbencho CSV row into a PhaseResult.

    Lives here (not in backends/elbencho.py) because the fio backend can
    reuse the same PhaseResult shape and only the input format differs.
    """
    m = row.metrics
    # On S3, treat IOPS column as ops/s; preserve POSIX iops field as null.
    iops_first = m.get("iops_first") if not is_s3 else None
    iops_last = m.get("iops_last") if not is_s3 else None
    ops_first = m.get("iops_first") if is_s3 else None
    ops_last = m.get("iops_last") if is_s3 else None
    return PhaseResult(
        operation=_classify_phase(row.operation),
        throughput_mib_s_first=m.get("mibps_first"),
        throughput_mib_s_last=m.get("mibps_last"),
        iops_first=iops_first,
        iops_last=iops_last,
        ops_per_s_first=ops_first,
        ops_per_s_last=ops_last,
        entries=m.get("entries_last") or m.get("entries_first"),
        mib_total=m.get("mib_total_last") or m.get("mib_total_first"),
        cpu_pct=m.get("cpu_pct_last") or m.get("cpu_pct_first"),
        io_lat_us=LatencyBucket(
            min=m.get("io_lat_us_min"),
            avg=m.get("io_lat_us_avg"),
            max=m.get("io_lat_us_max"),
        ),
        ent_lat_us=LatencyBucket(
            min=m.get("ent_lat_us_min"),
            avg=m.get("ent_lat_us_avg"),
            max=m.get("ent_lat_us_max"),
        ),
        latency_percentiles_us={},
        raw=row.raw,
    )


def build_result(
    *,
    run_spec: RunSpec,
    engine_name: str,
    engine_version: str | None,
    engine_features: list[str],
    engine_artifacts: EngineArtifactRefs,
    phases: dict[str, PhaseResult],
    started_at: datetime,
    finished_at: datetime,
    exit_code: int,
    primary_phase: str,
    stderr_tail: str = "",
) -> Result:
    """Package a parsed Result. Backend-agnostic.

    Each backend produces the `phases` dict and `engine_artifacts` references
    in whatever format its native output uses; this function just wraps them
    with the canonical run metadata.
    """
    target = run_spec.target
    if isinstance(target, PosixTarget):
        tgt_snap = TargetSnapshot(
            kind="posix",
            name=target.name,
            detail={
                "mount_path": str(target.mount_path),
                "dataset_subdir": target.dataset_subdir,
            },
        )
    elif isinstance(target, S3Target):
        tgt_snap = TargetSnapshot(
            kind="s3",
            name=target.name,
            detail={
                "endpoint": target.endpoint,
                "bucket": target.bucket,
                "region": target.region,
                "addressing": target.addressing,
            },
        )
    else:
        tgt_snap = TargetSnapshot(kind="unknown", name="?")

    wl = run_spec.workload
    workload_snap = WorkloadSnapshot(
        name=wl.name,
        block_size=wl.block_size,
        rw_mix_pct_read=wl.rw_mix_pct_read,
        threads_per_client=wl.threads_per_client,
        io_depth=wl.io_depth,
        pattern=wl.pattern,
        direct_io=wl.direct_io,
        duration_s=wl.duration_s,
        dataset_size=wl.dataset_size,
        file_size=wl.file_size,
        file_count=wl.file_count,
        total_concurrency=wl.threads_per_client * max(len(run_spec.clients), 1),
    )

    clients = [
        ClientInfo(host=c.host, elbencho_version=engine_version, features=engine_features)
        for c in run_spec.clients
    ]

    errors: list[str] = []
    if exit_code != 0:
        errors.append(f"{engine_name} exited {exit_code}")
        if stderr_tail:
            errors.append(stderr_tail[-2000:])

    duration_s = max(0.0, (finished_at - started_at).total_seconds())

    return Result(
        run_id=run_spec.run_id,
        spec_hash=run_spec.spec_hash,
        engine=engine_name,
        primary_phase=primary_phase,
        started_at=started_at,
        finished_at=finished_at,
        duration_s=duration_s,
        target=tgt_snap,
        workload=workload_snap,
        clients=clients,
        elbencho=engine_artifacts,
        phases=phases,
        elbencho_exit_code=exit_code,
        errors=errors,
    )
