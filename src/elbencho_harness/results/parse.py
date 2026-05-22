"""Assemble a canonical Result from elbencho's CSV + JSON output."""

from __future__ import annotations

from datetime import datetime
from pathlib import Path
from typing import Any

from ..config.models import PosixTarget, RunSpec, S3Target
from ..engine.elbencho import (
    ElbenchoArtifacts,
    ElbenchoVersion,
    PhaseRow,
    extract_percentiles,
    parse_csv,
    parse_jsonfile,
)
from .schema import (
    ClientInfo,
    ElbenchoArtifactRefs,
    LatencyBucket,
    PhaseResult,
    Result,
    TargetSnapshot,
    WorkloadSnapshot,
)


# Operations that elbencho emits as phase rows but which aren't actual IO
# (no MiB/s or IOPS, just directory / sync / cache prep work). They show up
# in raw/run.csv for completeness, but are excluded from the Result.phases
# dict so reports don't render null tiles.
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
    artifacts: ElbenchoArtifacts,
    version: ElbenchoVersion,
    command: str,
    started_at: datetime,
    finished_at: datetime,
    exit_code: int,
    primary_phase: str,
    stderr_tail: str = "",
) -> Result:
    rows = parse_csv(artifacts.csv)
    json_blob: dict[str, Any] | None = parse_jsonfile(artifacts.jsonfile)
    pct_by_label = extract_percentiles(json_blob)

    is_s3 = isinstance(run_spec.target, S3Target)
    phases: dict[str, PhaseResult] = {}
    for row in rows:
        phase = _phase_from_row(row, is_s3=is_s3)
        # Skip non-IO phases (mkdirs, sync, cleanup). They land in raw/run.csv
        # for the curious; the parsed Result.phases is for IO phases only.
        if phase.operation in NON_IO_OPERATIONS:
            continue
        # Merge percentiles by best matching label.
        for label, pcts in pct_by_label.items():
            if phase.operation in label.lower() or label.lower() in phase.operation:
                phase.latency_percentiles_us.update(pcts)
        phases.setdefault(phase.operation, phase)

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
                # credentials_ref kept out of result.json on purpose
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
        ClientInfo(host=c.host, elbencho_version=version.version, features=version.features)
        for c in run_spec.clients
    ]

    artifact_refs = ElbenchoArtifactRefs(
        command=command,
        csv_path=str(artifacts.csv),
        jsonfile_path=str(artifacts.jsonfile),
        stdout_path=str(artifacts.stdout),
        livecsv_path=str(artifacts.livecsv) if artifacts.livecsv.is_file() else None,
    )

    errors: list[str] = []
    if exit_code != 0:
        errors.append(f"elbencho exited {exit_code}")
        if stderr_tail:
            errors.append(stderr_tail[-2000:])

    duration_s = max(0.0, (finished_at - started_at).total_seconds())

    return Result(
        run_id=run_spec.run_id,
        spec_hash=run_spec.spec_hash,
        primary_phase=primary_phase,
        started_at=started_at,
        finished_at=finished_at,
        duration_s=duration_s,
        target=tgt_snap,
        workload=workload_snap,
        clients=clients,
        elbencho=artifact_refs,
        phases=phases,
        elbencho_exit_code=exit_code,
        errors=errors,
    )
