"""Canonical configuration models. Single source of truth for YAML and (future) TUI."""

from __future__ import annotations

import hashlib
import json
from enum import Enum
from pathlib import Path
from typing import Annotated, Any, Literal

import humanfriendly
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator


def _parse_bytes(v: Any) -> int:
    if isinstance(v, int):
        return v
    if isinstance(v, str):
        return int(humanfriendly.parse_size(v, binary=True))
    raise TypeError(f"cannot parse byte size from {type(v).__name__}: {v!r}")


ByteSize = Annotated[int, Field(ge=1)]


class TargetKind(str, Enum):
    POSIX = "posix"
    S3 = "s3"


class _StrictModel(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class PosixTarget(_StrictModel):
    kind: Literal["posix"] = "posix"
    name: str
    mount_path: Path
    dataset_subdir: str = "elbencho-bench"
    cleanup: bool = True

    @field_validator("dataset_subdir")
    @classmethod
    def _no_traversal(cls, v: str) -> str:
        if v.startswith("/") or ".." in Path(v).parts:
            raise ValueError("dataset_subdir must be a relative path with no '..'")
        return v


class S3Target(_StrictModel):
    kind: Literal["s3"] = "s3"
    name: str
    endpoint: str
    bucket: str
    region: str | None = None
    credentials_ref: str = Field(
        description="Reference to credentials, e.g. 'env:NAME' or 'file:/path'. Never inline."
    )
    addressing: Literal["path", "virtual"] = "path"

    @field_validator("credentials_ref")
    @classmethod
    def _no_inline_secrets(cls, v: str) -> str:
        if not (v.startswith("env:") or v.startswith("file:")):
            raise ValueError(
                "credentials_ref must be 'env:NAME' or 'file:/path'; inline secrets are rejected"
            )
        return v


Target = Annotated[PosixTarget | S3Target, Field(discriminator="kind")]


class Workload(_StrictModel):
    name: str
    pattern: Literal["seq", "rand"] = "seq"
    rw_mix_pct_read: int = Field(default=100, ge=0, le=100)
    block_size: ByteSize
    threads_per_client: int = Field(default=1, ge=1)
    io_depth: int = Field(default=1, ge=1)
    direct_io: bool = True
    sync_after_write: bool = False
    drop_caches_before: bool = False
    duration_s: int | None = Field(default=None, ge=1)
    dataset_size: ByteSize | None = None
    file_size: ByteSize | None = None
    file_count: int | None = Field(default=None, ge=1)
    # S3-only knobs. Ignored for POSIX targets.
    s3_multipart_size: ByteSize | None = None
    s3_object_prefix: str | None = None
    extra_flags: list[str] = Field(default_factory=list)

    @field_validator("block_size", "dataset_size", "file_size", "s3_multipart_size", mode="before")
    @classmethod
    def _coerce_bytes(cls, v: Any) -> int | None:
        if v is None:
            return None
        return _parse_bytes(v)

    @model_validator(mode="after")
    def _duration_xor_dataset(self) -> Workload:
        has_duration = self.duration_s is not None
        has_dataset = self.dataset_size is not None or self.file_size is not None
        if not has_duration and not has_dataset:
            raise ValueError(
                "workload must specify either duration_s OR (dataset_size and/or file_size)"
            )
        return self


class ClientHost(_StrictModel):
    """Reserved for v0.2+ multi-client SSH fan-out. v0.1 uses a single localhost entry."""

    host: str = "localhost"
    ssh_user: str | None = None
    ssh_port: int = 22
    ssh_key: Path | None = None
    elbencho_path: str = "elbencho"
    service_port: int = 1611


class SweepAxis(_StrictModel):
    """Reserved for v0.3+. v0.1 accepts the field but ignores non-base values."""

    block_size: list[ByteSize] | None = None
    rw_mix_pct_read: list[int] | None = None
    threads_per_client: list[int] | None = None
    io_depth: list[int] | None = None
    client_count: list[int] | None = None
    dataset_size: list[ByteSize] | None = None

    @field_validator("block_size", "dataset_size", mode="before")
    @classmethod
    def _coerce_bytes_list(cls, v: Any) -> list[int] | None:
        if v is None:
            return None
        return [_parse_bytes(x) for x in v]


class Sweep(_StrictModel):
    name: str
    base: str  # workload name reference
    targets: list[str] | None = None
    target: str | None = None
    axes: SweepAxis = Field(default_factory=SweepAxis)
    order: Literal["cartesian", "ladder"] = "cartesian"
    max_runs: int | None = Field(default=None, ge=1)


class RunRef(_StrictModel):
    target: str
    workload: str


class RunSpec(_StrictModel):
    """One concrete test. Built by the loader (or future sweep expander)."""

    run_id: str  # ULID, assigned at scheduling time
    spec_hash: str  # sha256 of normalized spec for resume / dedup
    target: Target
    workload: Workload
    clients: list[ClientHost] = Field(default_factory=lambda: [ClientHost()])

    @staticmethod
    def make_spec_hash(target: Target, workload: Workload, clients: list[ClientHost]) -> str:
        payload = {
            "target": target.model_dump(mode="json"),
            "workload": workload.model_dump(mode="json"),
            "clients": [c.model_dump(mode="json") for c in clients],
        }
        blob = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
        return "sha256:" + hashlib.sha256(blob).hexdigest()


class RunPlan(_StrictModel):
    """Top-level YAML config."""

    version: int = 1
    engine: Literal["elbencho", "fio"] = "elbencho"
    output_dir: Path = Path("./results")
    clients: list[ClientHost] = Field(default_factory=lambda: [ClientHost()])
    targets: list[Target]
    workloads: list[Workload]
    runs: list[RunRef] = Field(default_factory=list)
    sweeps: list[Sweep] = Field(default_factory=list)

    @model_validator(mode="after")
    def _validate_refs(self) -> RunPlan:
        target_names = {t.name for t in self.targets}
        workload_names = {w.name for w in self.workloads}
        if len(target_names) != len(self.targets):
            raise ValueError("duplicate target names")
        if len(workload_names) != len(self.workloads):
            raise ValueError("duplicate workload names")
        for i, r in enumerate(self.runs):
            if r.target not in target_names:
                raise ValueError(f"runs[{i}] references unknown target: {r.target}")
            if r.workload not in workload_names:
                raise ValueError(f"runs[{i}] references unknown workload: {r.workload}")
        for sw in self.sweeps:
            if sw.base not in workload_names:
                raise ValueError(f"sweep {sw.name!r} base workload not found: {sw.base}")
            sw_targets = sw.targets or ([sw.target] if sw.target else [])
            for tn in sw_targets:
                if tn not in target_names:
                    raise ValueError(f"sweep {sw.name!r} references unknown target: {tn}")
        return self

    def target_by_name(self, name: str) -> Target:
        for t in self.targets:
            if t.name == name:
                return t
        raise KeyError(f"unknown target: {name}")

    def workload_by_name(self, name: str) -> Workload:
        for w in self.workloads:
            if w.name == name:
                return w
        raise KeyError(f"unknown workload: {name}")
