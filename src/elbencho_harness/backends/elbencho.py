"""elbencho backend.

Wraps the original engine/elbencho.py command builder and CSV/JSON parser
behind the Backend protocol. Existing configs that don't declare an engine
continue to use this (it's the registry's default).
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from typing import ClassVar

from ..config.models import ClientHost, PosixTarget, RunSpec, S3Target, Target
from ..engine.elbencho import (
    artifacts_for,
    build_argv,
    parse_csv,
    parse_jsonfile,
)
from ..results.parse import NON_IO_OPERATIONS, _phase_from_row
from ..results.schema import EngineArtifactRefs, PhaseResult
from .base import EngineVersion, TargetSupport


class ElbenchoBackend:
    """elbencho-driven runs. Default engine; supports POSIX and S3 targets."""

    name: ClassVar[str] = "elbencho"

    def detect_version(self, local_path: str) -> EngineVersion:
        """elbencho's --version is a multi-line string with feature tokens."""
        proc = subprocess.run(
            [local_path, "--version"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        raw = (proc.stdout or "") + (proc.stderr or "")
        version: str | None = None
        features: list[str] = []
        m = re.search(r"version[:\s]+v?(\d+\.\d+(?:[.\-]\d+)?[^\s]*)", raw, re.IGNORECASE)
        if m:
            version = m.group(1)
        for feat in ("S3", "CUDA", "CUFILE"):
            if re.search(rf"\b{feat}\b", raw):
                features.append(feat)
        return EngineVersion(raw=raw.strip(), version=version, features=features)

    def build_argv(
        self,
        spec: RunSpec,
        raw_dir: Path,
        *,
        local_path: str,
        hosts: str | None = None,
    ) -> tuple[list[str], str]:
        artifacts = artifacts_for(raw_dir)
        return build_argv(spec, artifacts, elbencho_path=local_path, hosts=hosts)

    def parse_results(
        self, raw_dir: Path, *, command: str
    ) -> tuple[dict[str, PhaseResult], EngineArtifactRefs]:
        """Parse elbencho's CSV (primary) plus JSON (for percentiles)."""
        artifacts = artifacts_for(raw_dir)
        rows = parse_csv(artifacts.csv)
        # is_s3 doesn't change CSV layout, but it does change which ops field
        # carries the headline number. Inferred from target kind elsewhere; here
        # we assume POSIX semantics since the CSV columns are the same.
        is_s3 = False
        phases: dict[str, PhaseResult] = {}
        for row in rows:
            phase = _phase_from_row(row, is_s3=is_s3)
            if phase.operation in NON_IO_OPERATIONS:
                continue
            phases.setdefault(phase.operation, phase)
        # Best-effort percentile merge from --jsonfile output.
        json_blob = parse_jsonfile(artifacts.jsonfile)
        if json_blob:
            from ..engine.elbencho import extract_percentiles

            pct_by_label = extract_percentiles(json_blob)
            for label, pcts in pct_by_label.items():
                for phase_name, phase in phases.items():
                    if phase_name in label.lower() or label.lower() in phase_name:
                        phase.latency_percentiles_us.update(pcts)
        refs = EngineArtifactRefs(
            command=command,
            stdout_path=str(artifacts.stdout),
            csv_path=str(artifacts.csv),
            jsonfile_path=str(artifacts.jsonfile),
            livecsv_path=str(artifacts.livecsv) if artifacts.livecsv.is_file() else None,
        )
        return phases, refs

    def supports_target(self, target: Target) -> TargetSupport:
        # elbencho supports both POSIX and S3.
        if isinstance(target, (PosixTarget, S3Target)):
            return TargetSupport(supported=True)
        return TargetSupport(
            supported=False, reason=f"elbencho backend doesn't know target kind {type(target).__name__}"
        )

    def service_command(self, client: ClientHost) -> list[str]:
        return [client.elbencho_path, "--service", "--port", str(client.service_port)]


__all__ = ["ElbenchoBackend"]
