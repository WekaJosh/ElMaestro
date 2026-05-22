"""Tests for engine/service.py.

Mocks asyncssh and the TCP port probe so we don't need real network listeners.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from elbencho_harness.config.models import ClientHost
from elbencho_harness.engine import service as service_mod
from elbencho_harness.engine import ssh as ssh_mod
from elbencho_harness.engine.service import (
    ServiceEndpoint,
    ServiceError,
    hosts_arg,
    services_running,
)


@dataclass
class _Proc:
    terminated: bool = False

    def terminate(self) -> None:
        self.terminated = True

    async def wait_closed(self) -> None:
        return None


@dataclass
class _Conn:
    host: str
    port: int = 22
    username: str | None = None
    closed: bool = False
    commands_run: list[str] = field(default_factory=list)
    bg_commands: list[str] = field(default_factory=list)
    bg_procs: list[_Proc] = field(default_factory=list)
    version_stdout: str = "elbencho version 3.1.3\n"

    async def run(self, cmd: str, check: bool = False) -> Any:
        self.commands_run.append(cmd)
        from types import SimpleNamespace

        return SimpleNamespace(exit_status=0, stdout=self.version_stdout, stderr="")

    async def create_process(self, cmd: str) -> _Proc:
        self.bg_commands.append(cmd)
        p = _Proc()
        self.bg_procs.append(p)
        return p

    def close(self) -> None:
        self.closed = True

    async def wait_closed(self) -> None:
        return None


@pytest.fixture
def patch_ssh_and_probe(monkeypatch):
    """Replace asyncssh.connect and the TCP probe with cooperative fakes."""
    conns: list[_Conn] = []

    async def fake_connect(**kwargs: Any) -> _Conn:
        c = _Conn(host=kwargs["host"], port=kwargs.get("port", 22))
        conns.append(c)
        return c

    monkeypatch.setattr(ssh_mod.asyncssh, "connect", fake_connect)
    probed: list[tuple[str, int]] = []

    async def fake_probe(host: str, port: int, **kwargs: Any) -> bool:
        probed.append((host, port))
        return True

    monkeypatch.setattr(service_mod, "_probe_port", fake_probe)
    return conns, probed


@pytest.mark.asyncio
async def test_services_running_starts_and_stops_each_client(patch_ssh_and_probe):
    conns, probed = patch_ssh_and_probe
    clients = [
        ClientHost(host="h1", ssh_user="u", service_port=1611),
        ClientHost(host="h2", ssh_user="u", service_port=1611),
    ]
    async with services_running(clients) as endpoints:
        assert {e.host for e in endpoints} == {"h1", "h2"}
        assert all(e.port == 1611 for e in endpoints)
        # Each client should have had a service-start command and a version probe.
        all_bg = [c for c in conns for cmd in c.bg_commands]
        assert len(all_bg) == 2
        for c in conns:
            assert any("--service" in cmd and "1611" in cmd for cmd in c.bg_commands)
            assert any("--version" in cmd for cmd in c.commands_run)
        # Port probes happened.
        assert ("h1", 1611) in probed
        assert ("h2", 1611) in probed
    # On context exit the connections must be closed.
    for c in conns:
        assert c.closed is True
        for p in c.bg_procs:
            assert p.terminated is True


@pytest.mark.asyncio
async def test_services_running_propagates_probe_failure(monkeypatch):
    """If the port probe never succeeds, raise ServiceError; cleanup still runs."""

    async def fake_connect(**kwargs):
        return _Conn(host=kwargs["host"], port=kwargs.get("port", 22))

    monkeypatch.setattr(ssh_mod.asyncssh, "connect", fake_connect)

    async def never_listens(host: str, port: int, **kwargs) -> bool:
        return False

    monkeypatch.setattr(service_mod, "_probe_port", never_listens)
    # Speed up the test: shrink the wait loop.
    monkeypatch.setattr(
        service_mod,
        "_wait_for_service",
        _make_fast_waiter(),
    )
    with pytest.raises(ServiceError, match="never came up"):
        async with services_running([ClientHost(host="dead", ssh_user="u")]):
            pass


def _make_fast_waiter():
    async def fast(host: str, port: int, **kwargs):
        raise ServiceError(f"elbencho service never came up on {host}:{port}")

    return fast


def test_hosts_arg_formats_endpoints():
    eps = [
        ServiceEndpoint(host="h1", port=1611),
        ServiceEndpoint(host="h2", port=1612),
    ]
    assert hosts_arg(eps) == "h1:1611,h2:1612"
