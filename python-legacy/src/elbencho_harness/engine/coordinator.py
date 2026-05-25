"""Backend-agnostic coordinator.

Drives one RunSpec through either the local-only path or the SSH fan-out
path, dispatched on whether `clients == [localhost]`. The engine itself
(elbencho or fio) is selected by `RunPlan.engine` and resolved via the
backends registry; the coordinator only knows about the Backend protocol.

Two execution paths:
  - `run_locally`: single-host, no SSH. Used when clients == [localhost].
  - `run_fanout`:  multi-host. Starts the engine's service-mode process over
                   SSH on each remote ClientHost, runs the master locally
                   with --hosts (elbencho) or --client (fio) pointing at them.
"""

from __future__ import annotations

import asyncio
import os
import shlex
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from ..backends import Backend, get_backend
from ..config.models import ClientHost, PosixTarget, RunSpec, S3Target
from ..results.parse import build_result
from ..results.schema import Result
from .service import ServiceError, hosts_arg, services_running


class CoordinatorError(RuntimeError):
    pass


def _ensure_posix_dataset_dir(spec: RunSpec) -> None:
    tgt = spec.target
    if isinstance(tgt, PosixTarget):
        ds = tgt.mount_path / tgt.dataset_subdir
        ds.mkdir(parents=True, exist_ok=True)


def _local_path(spec: RunSpec) -> str:
    """Path to the engine binary on the local (coordinator) machine.

    For local-only runs this is clients[0].elbencho_path. For multi-client
    fan-out the local machine still runs the master process, so we need a
    binary here too; same field is reused.
    """
    if spec.clients:
        return spec.clients[0].elbencho_path
    return "elbencho"


def _is_localhost_only(clients: list[ClientHost]) -> bool:
    """True if the client list is just one entry pointing at this machine."""
    if len(clients) != 1:
        return False
    h = clients[0].host
    return h in {"localhost", "127.0.0.1", "::1", ""}


def run(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
    engine: str = "elbencho",
) -> Result:
    """Execute one RunSpec. Dispatches between local and fan-out automatically."""
    backend = get_backend(engine)
    if _is_localhost_only(spec.clients):
        return run_locally(spec, spec_dir=spec_dir, timeout_s=timeout_s, backend=backend)
    return run_fanout(spec, spec_dir=spec_dir, timeout_s=timeout_s, backend=backend)


def run_locally(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
    backend: Backend | None = None,
    engine: str = "elbencho",
) -> Result:
    """Execute one RunSpec on the local host."""
    backend = backend or get_backend(engine)
    raw_dir = spec_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    local_path = _local_path(spec)

    try:
        version = backend.detect_version(local_path)
    except FileNotFoundError as e:
        raise CoordinatorError(
            f"{backend.name} not found at {local_path!r}; install it and ensure "
            "it's in PATH (or set clients[0].elbencho_path)"
        ) from e

    support = backend.supports_target(spec.target)
    if not support.supported:
        raise CoordinatorError(support.reason)

    # elbencho-specific S3 feature check; other backends own their own gates.
    if backend.name == "elbencho" and isinstance(spec.target, S3Target) and not version.has("S3"):
        raise CoordinatorError(
            "elbencho was not built with S3_SUPPORT=1; rebuild with `make S3_SUPPORT=1` "
            "or use the breuner/elbencho Docker image"
        )

    _ensure_posix_dataset_dir(spec)

    argv, primary_phase = backend.build_argv(spec, raw_dir, local_path=local_path)
    command_str = " ".join(shlex.quote(p) for p in argv)

    env = os.environ.copy()
    if isinstance(spec.target, S3Target):
        _inject_s3_credentials(env, spec.target.credentials_ref)

    started = datetime.now(timezone.utc)
    proc = subprocess.run(
        argv,
        capture_output=True,
        text=True,
        env=env,
        timeout=timeout_s,
        check=False,
    )
    finished = datetime.now(timezone.utc)

    (raw_dir / "stdout.log").write_text(proc.stdout or "")
    if proc.stderr:
        (raw_dir / "stderr.log").write_text(proc.stderr)

    phases, artifact_refs = backend.parse_results(raw_dir, command=command_str)
    return build_result(
        run_spec=spec,
        engine_name=backend.name,
        engine_version=version.version,
        engine_features=version.features,
        engine_artifacts=artifact_refs,
        phases=phases,
        started_at=started,
        finished_at=finished,
        exit_code=proc.returncode,
        primary_phase=primary_phase,
        stderr_tail=proc.stderr or "",
    )


def run_fanout(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
    backend: Backend | None = None,
    engine: str = "elbencho",
) -> Result:
    """Multi-client run via SSH + the engine's service/server mode."""
    backend = backend or get_backend(engine)
    return asyncio.run(
        _run_fanout_async(spec, spec_dir=spec_dir, timeout_s=timeout_s, backend=backend)
    )


async def _run_fanout_async(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
    backend: Backend,
) -> Result:
    raw_dir = spec_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    local_path = _local_path(spec)

    try:
        version = backend.detect_version(local_path)
    except FileNotFoundError as e:
        raise CoordinatorError(
            f"local {backend.name} not found at {local_path!r}; the coordinator "
            "machine needs the binary too (it runs the master process)"
        ) from e

    support = backend.supports_target(spec.target)
    if not support.supported:
        raise CoordinatorError(support.reason)

    if backend.name == "elbencho" and isinstance(spec.target, S3Target) and not version.has("S3"):
        raise CoordinatorError(
            "elbencho was not built with S3_SUPPORT=1; rebuild with `make S3_SUPPORT=1`"
        )

    _ensure_posix_dataset_dir(spec)

    env = os.environ.copy()
    if isinstance(spec.target, S3Target):
        _inject_s3_credentials(env, spec.target.credentials_ref)

    try:
        async with services_running(spec.clients, service_command=backend.service_command) as endpoints:
            argv, primary_phase = backend.build_argv(
                spec, raw_dir, local_path=local_path, hosts=hosts_arg(endpoints)
            )
            command_str = " ".join(shlex.quote(p) for p in argv)
            started = datetime.now(timezone.utc)
            proc = await asyncio.to_thread(
                subprocess.run,
                argv,
                capture_output=True,
                text=True,
                env=env,
                timeout=timeout_s,
                check=False,
            )
            finished = datetime.now(timezone.utc)
    except ServiceError as e:
        raise CoordinatorError(f"service mode failed: {e}") from e

    (raw_dir / "stdout.log").write_text(proc.stdout or "")
    if proc.stderr:
        (raw_dir / "stderr.log").write_text(proc.stderr)

    phases, artifact_refs = backend.parse_results(raw_dir, command=command_str)
    return build_result(
        run_spec=spec,
        engine_name=backend.name,
        engine_version=version.version,
        engine_features=version.features,
        engine_artifacts=artifact_refs,
        phases=phases,
        started_at=started,
        finished_at=finished,
        exit_code=proc.returncode,
        primary_phase=primary_phase,
        stderr_tail=proc.stderr or "",
    )


def _inject_s3_credentials(env: dict[str, str], ref: str) -> None:
    """Resolve credentials_ref into AWS_* env vars for the subprocess."""
    if ref.startswith("env:"):
        name = ref[len("env:") :]
        if name not in env:
            raise CoordinatorError(f"credentials_ref points at env var {name!r} but it's unset")
        if "AWS_ACCESS_KEY_ID" not in env and ":" in env[name]:
            access, _, secret = env[name].partition(":")
            env["AWS_ACCESS_KEY_ID"] = access
            env["AWS_SECRET_ACCESS_KEY"] = secret
    elif ref.startswith("file:"):
        path = Path(ref[len("file:") :])
        if not path.is_file():
            raise CoordinatorError(f"credentials_ref file not found: {path}")
        content = path.read_text().strip()
        if ":" in content.splitlines()[0]:
            access, _, secret = content.splitlines()[0].partition(":")
        else:
            lines = content.splitlines()
            access, secret = lines[0], lines[1] if len(lines) > 1 else ""
        env["AWS_ACCESS_KEY_ID"] = access.strip()
        env["AWS_SECRET_ACCESS_KEY"] = secret.strip()
    else:
        raise CoordinatorError(f"unsupported credentials_ref scheme: {ref!r}")
