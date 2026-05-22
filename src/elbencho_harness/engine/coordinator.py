"""Subprocess coordinator.

Two execution paths:
  - `run_locally`: v0.1 single-host, no SSH. Used when clients == [localhost].
  - `run_fanout`:  v0.2 multi-host. Starts elbencho --service over SSH on each
                   remote ClientHost, then runs the master elbencho command on
                   the local machine with --hosts pointing at them.

Top-level entry is `run(spec, ...)` which picks the right path based on the
spec's client list.
"""

from __future__ import annotations

import asyncio
import os
import shlex
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from ..config.models import ClientHost, PosixTarget, RunSpec, S3Target
from ..results.parse import build_result
from ..results.schema import Result
from .elbencho import ElbenchoVersion, artifacts_for, build_argv, detect_version
from .service import ServiceError, hosts_arg, services_running


class CoordinatorError(RuntimeError):
    pass


def _ensure_posix_dataset_dir(spec: RunSpec) -> None:
    tgt = spec.target
    if isinstance(tgt, PosixTarget):
        ds = tgt.mount_path / tgt.dataset_subdir
        ds.mkdir(parents=True, exist_ok=True)


def _local_elbencho_path(spec: RunSpec) -> str:
    """The path to elbencho on the *local* (coordinator) machine.

    For single-client local runs this is just clients[0].elbencho_path. For
    multi-client fan-out the local machine still runs the master elbencho
    process, so we need a binary here too. We pick the first ClientHost's
    elbencho_path with the assumption that fleet binaries match; if your local
    install lives elsewhere, set client[0].elbencho_path accordingly.
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
) -> Result:
    """Execute one RunSpec. Dispatches between local and fan-out automatically."""
    if _is_localhost_only(spec.clients):
        return run_locally(spec, spec_dir=spec_dir, timeout_s=timeout_s)
    return run_fanout(spec, spec_dir=spec_dir, timeout_s=timeout_s)


def run_locally(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
) -> Result:
    """Execute one RunSpec on the local host. Writes raw artifacts under
    spec_dir/raw/ and returns a populated Result."""

    elbencho_path = _local_elbencho_path(spec)
    raw_dir = spec_dir / "raw"
    artifacts = artifacts_for(raw_dir)

    try:
        version: ElbenchoVersion = detect_version(elbencho_path)
    except FileNotFoundError as e:
        raise CoordinatorError(
            f"elbencho not found at {elbencho_path!r}; install it and ensure it's in PATH"
        ) from e

    if isinstance(spec.target, S3Target) and not version.has("S3"):
        raise CoordinatorError(
            "elbencho was not built with S3_SUPPORT=1; rebuild with `make S3_SUPPORT=1` "
            "or use the breuner/elbencho Docker image"
        )

    _ensure_posix_dataset_dir(spec)

    argv, primary_phase = build_argv(spec, artifacts, elbencho_path=elbencho_path)
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

    artifacts.stdout.write_text(proc.stdout or "")
    if proc.stderr:
        (raw_dir / "stderr.log").write_text(proc.stderr)

    return build_result(
        run_spec=spec,
        artifacts=artifacts,
        version=version,
        command=command_str,
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
) -> Result:
    """Multi-client run. Starts elbencho services on each client over SSH, then
    drives the master elbencho process locally with --hosts.

    Synchronous wrapper around the async core so the rest of the codebase
    doesn't have to be async-aware.
    """
    return asyncio.run(_run_fanout_async(spec, spec_dir=spec_dir, timeout_s=timeout_s))


async def _run_fanout_async(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
) -> Result:
    elbencho_path = _local_elbencho_path(spec)
    raw_dir = spec_dir / "raw"
    artifacts = artifacts_for(raw_dir)

    try:
        version: ElbenchoVersion = detect_version(elbencho_path)
    except FileNotFoundError as e:
        raise CoordinatorError(
            f"local elbencho not found at {elbencho_path!r}; the coordinator machine "
            "needs the binary too (it runs the master process)"
        ) from e

    if isinstance(spec.target, S3Target) and not version.has("S3"):
        raise CoordinatorError(
            "elbencho was not built with S3_SUPPORT=1; rebuild with `make S3_SUPPORT=1`"
        )

    _ensure_posix_dataset_dir(spec)

    env = os.environ.copy()
    if isinstance(spec.target, S3Target):
        _inject_s3_credentials(env, spec.target.credentials_ref)

    # Bring up services, run the master, tear down. The async-with handles
    # cleanup on either success or failure.
    try:
        async with services_running(spec.clients) as endpoints:
            argv, primary_phase = build_argv(
                spec, artifacts, elbencho_path=elbencho_path, hosts=hosts_arg(endpoints)
            )
            command_str = " ".join(shlex.quote(p) for p in argv)

            started = datetime.now(timezone.utc)
            # Run master subprocess in a thread so we don't block the event
            # loop (asyncio.create_subprocess_exec would be more idiomatic but
            # requires reworking timeouts and capture; this is simpler).
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

    artifacts.stdout.write_text(proc.stdout or "")
    if proc.stderr:
        (raw_dir / "stderr.log").write_text(proc.stderr)

    return build_result(
        run_spec=spec,
        artifacts=artifacts,
        version=version,
        command=command_str,
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
