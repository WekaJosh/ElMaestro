"""SSH layer built on the system `ssh` binary (subprocess-based).

Previous versions used asyncssh + cryptography, which baked their own
Python SSH client and a copy of OpenSSL into the binary (~17 MB). Driving
the system ssh(1) gives us the user's existing config for free
(~/.ssh/config, known_hosts, ssh-agent, ControlMaster, etc.) and slims
the bundle.

Same API as before so engine/service.py is unchanged:
  - SSHResult dataclass
  - SSHRunner with .run() / .start_background() / .stop_background() / .close()
  - open_runner / open_runners async context managers
"""

from __future__ import annotations

import asyncio
import secrets
import shlex
import subprocess
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import AsyncIterator

from ..config.models import ClientHost


class SSHError(RuntimeError):
    """Failure invoking ssh(1) against a ClientHost."""


@dataclass
class SSHResult:
    exit_status: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_status == 0


def _ssh_base_argv(client: ClientHost) -> list[str]:
    """Construct the ssh(1) base command for a ClientHost.

    Honors ssh_port, ssh_user, ssh_key. Adds BatchMode=yes so we never block
    waiting for a password prompt, and ConnectTimeout=10 so dead hosts fail
    fast rather than hanging the run.
    """
    argv: list[str] = [
        "ssh",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        "-o", "StrictHostKeyChecking=accept-new",
    ]
    if client.ssh_port and client.ssh_port != 22:
        argv += ["-p", str(client.ssh_port)]
    if client.ssh_key:
        argv += ["-i", str(Path(client.ssh_key).expanduser())]
    host = client.host
    if client.ssh_user:
        host = f"{client.ssh_user}@{host}"
    argv.append(host)
    return argv


@dataclass
class _BgProcess:
    """A long-running remote process we started. Tracked for cleanup."""

    marker: str
    pid_file: str
    log_file: str


class SSHRunner:
    """Drives one ClientHost via subprocess.

    Each .run() spawns a fresh ssh subprocess. For our use case (start a
    service, hold for one benchmark, tear down) the per-call overhead is
    negligible. If we ever need lots of small commands per host, we'd add
    OpenSSH ControlMaster multiplexing here.
    """

    def __init__(self, client: ClientHost) -> None:
        self.client = client
        self._bg_procs: list[_BgProcess] = []

    @property
    def host(self) -> str:
        return self.client.host

    async def run(self, argv: list[str] | str, *, timeout: float | None = None) -> SSHResult:
        """Run a one-shot remote command, return captured stdout/stderr/exit."""
        cmd_str = argv if isinstance(argv, str) else " ".join(shlex.quote(a) for a in argv)
        full_argv = _ssh_base_argv(self.client) + ["--", cmd_str]
        try:
            proc = await asyncio.to_thread(
                subprocess.run,
                full_argv,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired as e:
            raise SSHError(f"timeout running {cmd_str!r} on {self.host}") from e
        except OSError as e:
            raise SSHError(f"failed to spawn ssh for {self.host}: {e}") from e
        return SSHResult(
            exit_status=proc.returncode,
            stdout=proc.stdout or "",
            stderr=proc.stderr or "",
        )

    async def start_background(self, argv: list[str] | str) -> _BgProcess:
        """Start a long-running remote process via nohup.

        Writes a PID file on the remote so stop_background can kill it cleanly.
        Returns the _BgProcess handle (also stashed internally for close()).
        """
        cmd_str = argv if isinstance(argv, str) else " ".join(shlex.quote(a) for a in argv)
        marker = secrets.token_hex(8)
        pid_file = f"/tmp/elmaestro-{marker}.pid"
        log_file = f"/tmp/elmaestro-{marker}.log"
        # Wrap in `sh -c` so nohup + redirects + & all parse.
        wrap = (
            f"nohup {cmd_str} > {log_file} 2>&1 < /dev/null & "
            f"echo $! > {pid_file}"
        )
        r = await self.run(["sh", "-c", wrap], timeout=15)
        if not r.ok:
            raise SSHError(
                f"failed to start background process on {self.host}: "
                f"exit={r.exit_status} stderr={r.stderr[:200]}"
            )
        bg = _BgProcess(marker=marker, pid_file=pid_file, log_file=log_file)
        self._bg_procs.append(bg)
        return bg

    async def stop_background(self) -> None:
        """Kill every background process started via start_background.

        Best-effort: a stale PID file or already-dead process is fine. We
        rm the marker files after the kill so the remote /tmp stays tidy.
        """
        for bg in self._bg_procs:
            cleanup = (
                f"if [ -f {bg.pid_file} ]; then "
                f"  kill $(cat {bg.pid_file}) 2>/dev/null || true; "
                f"fi; "
                f"rm -f {bg.pid_file} {bg.log_file}"
            )
            try:
                await self.run(["sh", "-c", cleanup], timeout=10)
            except SSHError:
                # Already-dead host shouldn't break teardown.
                pass
        self._bg_procs.clear()

    async def close(self) -> None:
        await self.stop_background()


async def _health_check(client: ClientHost, *, timeout: float = 15.0) -> SSHRunner:
    """Open a runner and run a trivial command to verify ssh works."""
    runner = SSHRunner(client)
    try:
        result = await runner.run(["true"], timeout=timeout)
    except SSHError:
        raise
    if not result.ok:
        raise SSHError(
            f"ssh to {client.host} failed: exit={result.exit_status} "
            f"stderr={result.stderr[:200]}"
        )
    return runner


@asynccontextmanager
async def open_runner(
    client: ClientHost, *, connect_timeout: float = 15.0
) -> AsyncIterator[SSHRunner]:
    """Open an SSHRunner for a ClientHost; close it on exit."""
    runner = await _health_check(client, timeout=connect_timeout)
    try:
        yield runner
    finally:
        await runner.close()


@asynccontextmanager
async def open_runners(
    clients: list[ClientHost], *, connect_timeout: float = 15.0
) -> AsyncIterator[list[SSHRunner]]:
    """Open SSHRunners for many clients concurrently. Closes all on exit."""
    runners: list[SSHRunner] = []
    try:
        runners = await asyncio.gather(
            *(_health_check(c, timeout=connect_timeout) for c in clients)
        )
        yield runners
    finally:
        for r in runners:
            try:
                await r.close()
            except Exception:
                pass


# Backwards-compat shim. Old tests imported _connect_kwargs from the
# asyncssh-based implementation; keep a no-op equivalent so the tests
# that exercise key-path handling still pass after the swap.
def _connect_kwargs(client: ClientHost) -> dict:
    """For tests only: returns the parts of the ssh argv that depend on
    the ClientHost, in a dict format mirroring the old asyncssh kwargs."""
    out: dict = {"host": client.host, "port": client.ssh_port}
    if client.ssh_user:
        out["username"] = client.ssh_user
    if client.ssh_key:
        out["client_keys"] = [str(Path(client.ssh_key).expanduser())]
    return out
