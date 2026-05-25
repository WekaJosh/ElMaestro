"""elbencho service-mode lifecycle.

Drives the multi-client benchmark dance:
  1. SSH to each ClientHost in parallel.
  2. Start `elbencho --service --port <P>` as a background process on each.
  3. Probe the service port until it's accepting connections (or time out).
  4. Yield the list of (host, port) tuples so the master can use --hosts.
  5. On context exit, terminate all services and close SSH.

The master itself runs locally (the machine running `bench run`) so result
artifacts land in the local output directory without an scp round trip.
"""

from __future__ import annotations

import asyncio
import socket
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import AsyncIterator, Callable

from ..config.models import ClientHost
from .ssh import SSHError, SSHRunner, open_runners


@dataclass
class ServiceEndpoint:
    host: str
    port: int
    elbencho_version: str | None = None

    def as_hosts_arg(self) -> str:
        return f"{self.host}:{self.port}"


class ServiceError(RuntimeError):
    """Service lifecycle failure (start, probe, version mismatch)."""


async def _probe_port(host: str, port: int, *, timeout: float = 1.0) -> bool:
    """One TCP-connect probe. True if the port accepted the connection."""
    loop = asyncio.get_running_loop()
    try:
        # asyncio's open_connection wraps this with a transport, but we just want
        # a quick reachability check without negotiating anything.
        await asyncio.wait_for(
            loop.run_in_executor(None, _sync_probe, host, port, timeout),
            timeout=timeout + 0.5,
        )
        return True
    except (OSError, asyncio.TimeoutError):
        return False


def _sync_probe(host: str, port: int, timeout: float) -> None:
    with socket.create_connection((host, port), timeout=timeout):
        pass


async def _wait_for_service(
    host: str, port: int, *, attempts: int = 30, interval: float = 0.5
) -> None:
    for _ in range(attempts):
        if await _probe_port(host, port):
            return
        await asyncio.sleep(interval)
    raise ServiceError(f"elbencho service never came up on {host}:{port}")


async def _detect_remote_version(runner: SSHRunner) -> str | None:
    """Best-effort: ask the remote elbencho for its version. Returns None on failure."""
    try:
        r = await runner.run([runner.client.elbencho_path, "--version"], timeout=10)
    except SSHError:
        return None
    if not r.ok:
        return None
    import re

    m = re.search(r"version[:\s]+v?(\d+\.\d+(?:\.\d+)?[^\s]*)", r.stdout + r.stderr, re.IGNORECASE)
    return m.group(1) if m else None


_DefaultServiceCmd = Callable[[ClientHost], list[str]]


def _default_elbencho_service_cmd(client: ClientHost) -> list[str]:
    return [client.elbencho_path, "--service", "--port", str(client.service_port)]


async def _start_one(
    runner: SSHRunner, service_command: _DefaultServiceCmd
) -> ServiceEndpoint:
    """Start the engine's service-mode process on one host and wait until it's listening."""
    client = runner.client
    version = await _detect_remote_version(runner)
    cmd = service_command(client)
    try:
        await runner.start_background(cmd)
    except SSHError as e:
        raise ServiceError(f"failed to start service on {client.host}: {e}") from e
    try:
        await _wait_for_service(client.host, client.service_port)
    except ServiceError:
        # Tear down the half-started process so we don't leak it.
        await runner.stop_background()
        raise
    return ServiceEndpoint(host=client.host, port=client.service_port, elbencho_version=version)


@asynccontextmanager
async def services_running(
    clients: list[ClientHost],
    *,
    connect_timeout: float = 15.0,
    service_command: _DefaultServiceCmd = _default_elbencho_service_cmd,
) -> AsyncIterator[list[ServiceEndpoint]]:
    """Bring up service-mode processes on all clients, yield endpoints, tear down on exit.

    `service_command` is a callable that returns the argv for one client's
    service-mode process. Default builds the elbencho command. Pass
    backend.service_command for engine-agnostic dispatch.

    Concurrency: all clients connect and start in parallel. If any single one
    fails, we tear down the rest and raise.
    """
    async with open_runners(clients, connect_timeout=connect_timeout) as runners:
        endpoints = await asyncio.gather(*(_start_one(r, service_command) for r in runners))
        try:
            yield endpoints
        finally:
            # open_runners already calls runner.close() which terminates bg procs,
            # so we don't need to do anything explicit here. We do want to give
            # the services a beat to drain the engine's final stats output.
            await asyncio.sleep(0.1)


def hosts_arg(endpoints: list[ServiceEndpoint]) -> str:
    """Format endpoints for elbencho's --hosts flag."""
    return ",".join(e.as_hosts_arg() for e in endpoints)
