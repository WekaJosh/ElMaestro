"""On-disk layout for run results."""

from __future__ import annotations

import json
import re
from datetime import datetime, timezone
from pathlib import Path

from pydantic import BaseModel, Field

from .schema import Result

SAFE_NAME = re.compile(r"[^A-Za-z0-9._-]+")


def _slug(s: str) -> str:
    return SAFE_NAME.sub("-", s).strip("-") or "x"


class Manifest(BaseModel):
    """Top-level manifest for a run directory."""

    schema_version: str = "1.0"
    run_id: str
    created_at: datetime
    run_specs: list[dict] = Field(default_factory=list)
    statuses: dict[str, str] = Field(default_factory=dict)  # spec_hash -> status


def new_run_dir(base: Path, label: str) -> Path:
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%S")
    d = base / f"{ts}_{_slug(label)}"
    d.mkdir(parents=True, exist_ok=True)
    return d


def spec_dir(
    run_dir: Path,
    index: int,
    target_name: str,
    workload_name: str,
    label: str | None = None,
) -> Path:
    """Filesystem layout for one RunSpec.

    `label` is an optional sweep-point suffix like 'bs=1MiB_t=4'. When provided,
    it lands after the workload name so directory listings sort sensibly and
    you can grep for the axis values without reading manifest.json.
    """
    parts = [f"{index:04d}", _slug(target_name), _slug(workload_name)]
    if label:
        parts.append(_slug(label))
    d = run_dir / "_".join(parts)
    (d / "raw").mkdir(parents=True, exist_ok=True)
    return d


def write_result(spec_path: Path, result: Result) -> Path:
    out = spec_path / "result.json"
    out.write_text(result.model_dump_json(indent=2))
    return out


def read_result(spec_path: Path) -> Result:
    return Result.model_validate_json((spec_path / "result.json").read_text())


def write_manifest(run_dir: Path, manifest: Manifest) -> Path:
    out = run_dir / "manifest.json"
    out.write_text(manifest.model_dump_json(indent=2))
    return out


def read_manifest(run_dir: Path) -> Manifest:
    return Manifest.model_validate_json((run_dir / "manifest.json").read_text())


def list_results(run_dir: Path) -> list[Result]:
    out: list[Result] = []
    for child in sorted(run_dir.iterdir()):
        rj = child / "result.json"
        if rj.is_file():
            try:
                out.append(Result.model_validate_json(rj.read_text()))
            except Exception:
                continue
    return out


def write_json(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=2, default=str))
