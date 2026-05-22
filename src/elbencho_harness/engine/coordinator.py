"""Local subprocess coordinator. v0.1: drives elbencho on this host only.

v0.2 will add an SSH fan-out path that starts `elbencho --service` on remote
clients and runs the coordinator command locally with --hosts.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from ..config.models import PosixTarget, RunSpec
from ..results.parse import build_result
from ..results.schema import Result
from .elbencho import ElbenchoVersion, artifacts_for, build_argv, detect_version


class CoordinatorError(RuntimeError):
    pass


def _ensure_posix_dataset_dir(spec: RunSpec) -> None:
    tgt = spec.target
    if isinstance(tgt, PosixTarget):
        ds = tgt.mount_path / tgt.dataset_subdir
        ds.mkdir(parents=True, exist_ok=True)


def _resolve_elbencho_path(spec: RunSpec) -> str:
    """v0.1: a single (local) client. Honor its elbencho_path."""
    if spec.clients:
        return spec.clients[0].elbencho_path
    return "elbencho"


def run_locally(
    spec: RunSpec,
    *,
    spec_dir: Path,
    timeout_s: int | None = None,
) -> Result:
    """Execute one RunSpec on the local host. Writes raw artifacts under
    spec_dir/raw/ and returns a populated Result."""

    elbencho_path = _resolve_elbencho_path(spec)
    raw_dir = spec_dir / "raw"
    artifacts = artifacts_for(raw_dir)

    # Preflight: version + feature detection. Raises if elbencho is missing.
    try:
        version: ElbenchoVersion = detect_version(elbencho_path)
    except FileNotFoundError as e:
        raise CoordinatorError(
            f"elbencho not found at {elbencho_path!r}; install it and ensure it's in PATH"
        ) from e

    # S3 needs the S3 feature compiled in; fail-fast.
    from ..config.models import S3Target

    if isinstance(spec.target, S3Target) and not version.has("S3"):
        raise CoordinatorError(
            "elbencho was not built with S3_SUPPORT=1; rebuild with `make S3_SUPPORT=1` "
            "or use the breuner/elbencho Docker image"
        )

    _ensure_posix_dataset_dir(spec)

    argv, primary_phase = build_argv(spec, artifacts, elbencho_path=elbencho_path)
    command_str = " ".join(shlex.quote(p) for p in argv)

    # Inject S3 credentials from credentials_ref, if applicable.
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


def _inject_s3_credentials(env: dict[str, str], ref: str) -> None:
    """Resolve credentials_ref into AWS_* env vars for the subprocess."""
    if ref.startswith("env:"):
        # Already in our env. Validate that the named var is set.
        name = ref[len("env:") :]
        if name not in env:
            raise CoordinatorError(f"credentials_ref points at env var {name!r} but it's unset")
        # Assume convention: var is a JSON-ish 'access:secret' pair OR the user
        # already populated AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY themselves.
        # If the env var has a colon in it, split.
        if "AWS_ACCESS_KEY_ID" not in env and ":" in env[name]:
            access, _, secret = env[name].partition(":")
            env["AWS_ACCESS_KEY_ID"] = access
            env["AWS_SECRET_ACCESS_KEY"] = secret
    elif ref.startswith("file:"):
        path = Path(ref[len("file:") :])
        if not path.is_file():
            raise CoordinatorError(f"credentials_ref file not found: {path}")
        content = path.read_text().strip()
        # Accept simple `access:secret` or two lines.
        if ":" in content.splitlines()[0]:
            access, _, secret = content.splitlines()[0].partition(":")
        else:
            lines = content.splitlines()
            access, secret = lines[0], lines[1] if len(lines) > 1 else ""
        env["AWS_ACCESS_KEY_ID"] = access.strip()
        env["AWS_SECRET_ACCESS_KEY"] = secret.strip()
    else:
        raise CoordinatorError(f"unsupported credentials_ref scheme: {ref!r}")
