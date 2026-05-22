"""fio backend.

Drives the [fio](https://github.com/axboe/fio) workload generator. Supports
POSIX targets locally and across SSH-fanned-out workers via fio's native
client/server protocol. S3 targets are deferred (fio's S3 engines are less
mature than elbencho's; tracked in docs/PLAN.md).

Command construction strategy: build a tiny fio job-file on disk under the
spec's raw/ directory, then invoke fio referencing that file. The job file
is captured for forensics alongside the JSON output.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from typing import ClassVar

from ..config.models import ClientHost, PosixTarget, RunSpec, S3Target, Target
from ..results.schema import EngineArtifactRefs, LatencyBucket, PhaseResult
from .base import EngineVersion, TargetSupport


# Map (pattern, rw_mix_pct_read) to fio's --rw value.
def _fio_rw(pattern: str, rw_mix_pct_read: int) -> str:
    if pattern == "rand":
        if rw_mix_pct_read == 100:
            return "randread"
        if rw_mix_pct_read == 0:
            return "randwrite"
        return "randrw"
    # sequential
    if rw_mix_pct_read == 100:
        return "read"
    if rw_mix_pct_read == 0:
        return "write"
    return "rw"


def _primary_phase_for(rw_mix_pct_read: int) -> str:
    if rw_mix_pct_read == 100:
        return "read"
    if rw_mix_pct_read == 0:
        return "write"
    return "mixed"


class FioBackend:
    name: ClassVar[str] = "fio"

    def detect_version(self, local_path: str) -> EngineVersion:
        """fio --version emits a single line like 'fio-3.36'."""
        proc = subprocess.run(
            [local_path, "--version"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        raw = (proc.stdout or proc.stderr or "").strip()
        version: str | None = None
        m = re.search(r"fio[-\s]+(\d+\.\d+(?:\.\d+)?)", raw)
        if m:
            version = m.group(1)
        return EngineVersion(raw=raw, version=version, features=[])

    def build_argv(
        self,
        spec: RunSpec,
        raw_dir: Path,
        *,
        local_path: str,
        hosts: str | None = None,
    ) -> tuple[list[str], str]:
        """Construct fio command line and write the job file to raw_dir."""
        wl = spec.workload
        tgt = spec.target
        if not isinstance(tgt, PosixTarget):
            raise ValueError("fio backend only supports POSIX targets in v0.7")

        dataset_dir = tgt.mount_path / tgt.dataset_subdir
        rw = _fio_rw(wl.pattern, wl.rw_mix_pct_read)
        primary_phase = _primary_phase_for(wl.rw_mix_pct_read)

        # Job file. We generate it deterministically so re-runs against the
        # same RunSpec produce identical jobs.
        job_lines: list[str] = []
        job_lines.append(f"[{wl.name}]")
        job_lines.append("ioengine=psync")
        job_lines.append(f"directory={dataset_dir}")
        job_lines.append(f"rw={rw}")
        job_lines.append(f"bs={wl.block_size}")
        job_lines.append(f"iodepth={wl.io_depth}")
        job_lines.append(f"numjobs={wl.threads_per_client}")
        if wl.file_size is not None:
            job_lines.append(f"size={wl.file_size}")
        if wl.file_count is not None and wl.file_count > 1:
            job_lines.append(f"nrfiles={wl.file_count}")
        if wl.direct_io:
            job_lines.append("direct=1")
        if wl.sync_after_write:
            job_lines.append("end_fsync=1")
        if wl.duration_s is not None:
            job_lines.append(f"runtime={wl.duration_s}")
            job_lines.append("time_based=1")
        if 0 < wl.rw_mix_pct_read < 100:
            job_lines.append(f"rwmixread={wl.rw_mix_pct_read}")
        # Always aggregate per-job stats so the JSON has one entry per
        # workload, not one per thread.
        job_lines.append("group_reporting=1")
        # Append user-supplied raw fio options.
        for flag in wl.extra_flags:
            job_lines.append(flag)

        job_file = raw_dir / "job.fio"
        job_file.write_text("\n".join(job_lines) + "\n", encoding="utf-8")

        json_out = raw_dir / "run.json"

        argv: list[str] = [local_path]
        argv += ["--output-format=json", f"--output={json_out}"]
        # Multi-client: append --client= for each fanned-out worker. fio
        # ignores command-line job options after --client, so the workers
        # read the job file we pass at the end.
        if hosts:
            for hp in hosts.split(","):
                argv.append(f"--client={hp}")
        argv.append(str(job_file))

        return argv, primary_phase

    def parse_results(
        self, raw_dir: Path, *, command: str
    ) -> tuple[dict[str, PhaseResult], EngineArtifactRefs]:
        """Parse fio's --output-format=json into PhaseResult entries."""
        import json

        json_path = raw_dir / "run.json"
        stdout_path = raw_dir / "stdout.log"
        phases: dict[str, PhaseResult] = {}
        if json_path.is_file():
            try:
                data = json.loads(json_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, OSError):
                data = None
            if isinstance(data, dict):
                phases = self._phases_from_fio_json(data)
        refs = EngineArtifactRefs(
            command=command,
            stdout_path=str(stdout_path),
            jsonfile_path=str(json_path),
        )
        return phases, refs

    @staticmethod
    def _phases_from_fio_json(data: dict) -> dict[str, PhaseResult]:
        """Map fio's JSON output to canonical PhaseResult shapes.

        fio JSON structure (abridged):
          { "jobs": [
              { "jobname": "...",
                "read":  { "bw":..., "iops":..., "lat_ns": {...},
                           "clat_ns": { "min", "max", "mean",
                                        "percentile": {"50.000000":..., ...} } },
                "write": { ... same shape ... },
                "mixed": { ... when rwmixread set ... }, ... } ] }

        Multi-client runs add "client_stats" as well; we ignore those here
        since the master already aggregates into "jobs[*]".
        """
        out: dict[str, PhaseResult] = {}
        jobs = data.get("jobs") or []
        if not jobs:
            return out
        # We expect exactly one job (group_reporting=1). If multiple, take
        # the first; the user can inspect raw/run.json for the per-job split.
        job = jobs[0]
        for fio_op in ("read", "write", "mixed"):
            section = job.get(fio_op)
            if not isinstance(section, dict):
                continue
            if not section.get("io_bytes") and not section.get("iops"):
                # fio emits empty sections for the unused side of pure read /
                # pure write; skip those so they don't render null tiles.
                continue
            out[fio_op] = _phase_from_fio_section(fio_op, section)
        return out

    def supports_target(self, target: Target) -> TargetSupport:
        if isinstance(target, PosixTarget):
            return TargetSupport(supported=True)
        if isinstance(target, S3Target):
            return TargetSupport(
                supported=False,
                reason=(
                    "fio's S3 ioengines are weaker than elbencho's; v0.7 keeps "
                    "S3 on the elbencho backend. Set `engine: elbencho` in your "
                    "config for S3 targets."
                ),
            )
        return TargetSupport(
            supported=False, reason=f"fio backend doesn't know target kind {type(target).__name__}"
        )

    def service_command(self, client: ClientHost) -> list[str]:
        # `fio --server=,N:<port>` binds to all interfaces on the given port.
        return [client.elbencho_path, f"--server=,N:{client.service_port}"]


def _phase_from_fio_section(op: str, sec: dict) -> PhaseResult:
    """Translate one fio job's read/write/mixed dict into a PhaseResult.

    Units conversion:
      - fio bw is KiB/s -> MiB/s (divide by 1024)
      - fio latency is ns -> µs (divide by 1000)
    """
    bw_kib_s = float(sec.get("bw") or 0)
    iops = sec.get("iops")
    iops_n = float(iops) if iops is not None else None
    tput_mib_s = bw_kib_s / 1024.0 if bw_kib_s else None

    clat_ns = sec.get("clat_ns") or {}
    lat_min_us = (clat_ns.get("min") or 0) / 1000.0 if clat_ns.get("min") is not None else None
    lat_max_us = (clat_ns.get("max") or 0) / 1000.0 if clat_ns.get("max") is not None else None
    lat_avg_us = (clat_ns.get("mean") or 0) / 1000.0 if clat_ns.get("mean") is not None else None

    pct_us: dict[str, float] = {}
    pct_section = clat_ns.get("percentile") or {}
    for pct_key, val_ns in pct_section.items():
        # fio gives keys like "50.000000", "99.000000", "99.900000"; normalize.
        try:
            pct_num = float(pct_key)
            val_us = float(val_ns) / 1000.0
        except (TypeError, ValueError):
            continue
        # "99.000000" -> "p99", "99.900000" -> "p99.9"
        label = f"p{pct_num:g}".replace(".0", "") if pct_num == int(pct_num) else f"p{pct_num:g}"
        pct_us[label] = val_us

    bytes_total = sec.get("io_bytes")
    mib_total = float(bytes_total) / (1024 * 1024) if bytes_total else None
    iops_mean = sec.get("iops_mean")
    if iops_mean is not None and iops_n is None:
        iops_n = float(iops_mean)

    return PhaseResult(
        operation=op,
        throughput_mib_s_first=tput_mib_s,
        throughput_mib_s_last=tput_mib_s,
        iops_first=iops_n,
        iops_last=iops_n,
        mib_total=mib_total,
        io_lat_us=LatencyBucket(min=lat_min_us, avg=lat_avg_us, max=lat_max_us),
        latency_percentiles_us=pct_us,
        raw={k: v for k, v in sec.items() if not isinstance(v, (dict, list))},
    )


__all__ = ["FioBackend"]
