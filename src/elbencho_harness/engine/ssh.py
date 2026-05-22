"""asyncssh wrapper for the multi-client coordinator.

This is the ONLY module that imports asyncssh. Everything else talks to it
through SSHRunner / open_runner so we can keep the surface area small and the
mocking points obvious.
"""

from __future__ import annotations

import asyncio
import shlex
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import AsyncIterator

import asyncssh

from ..config.models import ClientHost


class SSHError(RuntimeError):
    """Failure talking to a remote ClientHost over SSH."""


@dataclass
class SSHResult:
    exit_status: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_status == 0


class SSHRunner:
    """Wraps an active SSHClientConnection. One per ClientHost.

    Use `open_runner` instead of constructing directly so the lifecycle is
    managed via async-with.
    """

    def __init__(self, client: ClientHost, conn: asyncssh.SSHClientConnection):
        self.client = client
        self._conn = conn
        # Long-running processes (e.g. elbencho --service) get stashed here so
        # they stay open for the lifetime of the connection. Close the runner
        # to tear them down.
        self._bg_procs: list[asyncssh.SSHClientProcess] = []

    @property
    def host(self) -> str:
        return self.client.host

    async def run(self, argv: list[str] | str, *, timeout: float | None = None) -> SSHResult:
        """Run a one-shot command, wait for completion, return captured output."""
        cmd = argv if isinstance(argv, str) else " ".join(shlex.quote(a) for a in argv)
        try:
            result = await asyncio.wait_for(
                self._conn.run(cmd, check=False), timeout=timeout
            )
        except asyncio.TimeoutError as e:
            raise SSHError(f"timeout running {cmd!r} on {self.host}") from e
        except asyncssh.Error as e:
            raise SSHError(f"ssh error running {cmd!r} on {self.host}: {e}") from e
        return SSHResult(
            exit_status=int(result.exit_status or 0),
            stdout=str(result.stdout or ""),
            stderr=str(result.stderr or ""),
        )

    async def start_background(self, argv: list[str] | str) -> asyncssh.SSHClientProcess:
        """Spawn a process that should stay alive for the lifetime of the runner.

        The process is killed when the connection closes (asyncssh sends EOF +
        SIGHUP). Callers can also stop it explicitly with `stop_background`.
        """
        cmd = argv if isinstance(argv, str) else " ".join(shlex.quote(a) for a in argv)
        try:
            proc = await self._conn.create_process(cmd)
        except asyncssh.Error as e:
            raise SSHError(f"ssh error spawning {cmd!r} on {self.host}: {e}") from e
        self._bg_procs.append(proc)
        return proc

    async def stop_background(self) -> None:
        """Terminate all background processes started via start_background."""
        for proc in self._bg_procs:
            try:
                proc.terminate()
            except OSError:
                pass
        # Give them a chance to exit cleanly.
        for proc in self._bg_procs:
            try:
                await asyncio.wait_for(proc.wait_closed(), timeout=5.0)
            except (asyncio.TimeoutError, OSError):
                pass
        self._bg_procs.clear()

    async def close(self) -> None:
        await self.stop_background()
        self._conn.close()
        await self._conn.wait_closed()


def _connect_kwargs(client: ClientHost) -> dict:
    """Translate a ClientHost into kwargs for asyncssh.connect.

    SSH key strategy: if `ssh_key` is set, use that explicitly. Otherwise fall
    back to asyncssh's defaults (agent + ~/.ssh keys). We never enable
    password auth.
    """
    kwargs: dict = {
        "host": client.host,
        "port": client.ssh_port,
        "username": client.ssh_user,
        "known_hosts": None,  # POV laptops rarely have a curated known_hosts
        "client_keys": None,
    }
    if client.ssh_key is not None:
        key_path = Path(client.ssh_key).expanduser()
        kwargs["client_keys"] = [str(key_path)]
    # Drop Nones so asyncssh uses defaults where appropriate.
    return {k: v for k, v in kwargs.items() if v is not None}


@asynccontextmanager
async def open_runner(
    client: ClientHost, *, connect_timeout: float = 15.0
) -> AsyncIterator[SSHRunner]:
    """Open an SSHRunner for a ClientHost. Closes it on exit.

    localhost shortcut: we still go through SSH so the code paths stay uniform
    and tests are honest. If a deployment wants a faster localhost path, the
    coordinator's `run_locally` already covers that case.
    """
    kwargs = _connect_kwargs(client)
    try:
        conn = await asyncio.wait_for(asyncssh.connect(**kwargs), timeout=connect_timeout)
    except asyncio.TimeoutError as e:
        raise SSHError(f"timeout connecting to {client.host}:{client.ssh_port}") from e
    except (OSError, asyncssh.Error) as e:
        raise SSHError(f"failed to connect to {client.host}:{client.ssh_port}: {e}") from e
    runner = SSHRunner(client, conn)
    try:
        yield runner
    finally:
        await runner.close()


@asynccontextmanager
async def open_runners(
    clients: list[ClientHost], *, connect_timeout: float = 15.0
) -> AsyncIterator[list[SSHRunner]]:
    """Open SSHRunners for many clients concurrently. Closes all on exit.

    If any single client fails to connect we close the ones that succeeded and
    raise. Partial-fanout retry logic lives in `engine/service.py`.
    """
    runners: list[SSHRunner] = []
    try:
        coros = [_open_one(c, connect_timeout) for c in clients]
        runners = await asyncio.gather(*coros)
        yield runners
    finally:
        for r in runners:
            try:
                await r.close()
            except Exception:
                pass


async def _open_one(client: ClientHost, connect_timeout: float) -> SSHRunner:
    """Helper used by open_runners. Not part of the public API."""
    kwargs = _connect_kwargs(client)
    try:
        conn = await asyncio.wait_for(asyncssh.connect(**kwargs), timeout=connect_timeout)
    except asyncio.TimeoutError as e:
        raise SSHError(f"timeout connecting to {client.host}:{client.ssh_port}") from e
    except (OSError, asyncssh.Error) as e:
        raise SSHError(f"failed to connect to {client.host}:{client.ssh_port}: {e}") from e
    return SSHRunner(client, conn)
