"""Tests for engine/service.py.

Service lifecycle is driven through the SSH layer (now subprocess-based).
We mock subprocess.run + the TCP port probe so no real ssh / no real
listeners are needed.
"""

from __future__ import annotations

from dataclasses import dataclass

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
class _CompletedProcess:
    returncode: int = 0
    stdout: str = ""
    stderr: str = ""


def _patch_subprocess(monkeypatch, returncode: int = 0, stdout: str = "elbencho 3.1.3\n"):
    """Make subprocess.run record argv and return a configurable result."""
    calls: list[list[str]] = []

    def fake_run(argv, **kwargs):
        calls.append(list(argv))
        return _CompletedProcess(returncode=returncode, stdout=stdout, stderr="")

    monkeypatch.setattr(ssh_mod.subprocess, "run", fake_run)
    return calls


@pytest.fixture
def patch_ssh_and_probe(monkeypatch):
    """Replace subprocess.run and the TCP probe with cooperative fakes."""
    calls = _patch_subprocess(monkeypatch)
    probed: list[tuple[str, int]] = []

    async def fake_probe(host: str, port: int, **kwargs) -> bool:
        probed.append((host, port))
        return True

    monkeypatch.setattr(service_mod, "_probe_port", fake_probe)
    return calls, probed


@pytest.mark.asyncio
async def test_services_running_starts_and_stops_each_client(patch_ssh_and_probe):
    calls, probed = patch_ssh_and_probe
    clients = [
        ClientHost(host="h1", ssh_user="u", service_port=1611),
        ClientHost(host="h2", ssh_user="u", service_port=1611),
    ]
    async with services_running(clients) as endpoints:
        assert {e.host for e in endpoints} == {"h1", "h2"}
        assert all(e.port == 1611 for e in endpoints)
        # Each client must have had a remote `nohup ... --service ... --port 1611` invocation.
        nohup_cmds = [c[-1] for c in calls if "nohup" in c[-1]]
        assert any("--service" in cmd and "1611" in cmd for cmd in nohup_cmds)
        # Each client must have had a `--version` probe.
        version_cmds = [c[-1] for c in calls if "--version" in c[-1]]
        assert len(version_cmds) >= 2
        # Port probes happened.
        assert ("h1", 1611) in probed
        assert ("h2", 1611) in probed
    # On context exit, the cleanup `kill $(cat ...pid)` must have run for each.
    cleanup_cmds = [c[-1] for c in calls if "kill" in c[-1]]
    assert len(cleanup_cmds) >= 2


@pytest.mark.asyncio
async def test_services_running_propagates_probe_failure(monkeypatch):
    """If the port probe never succeeds, raise ServiceError; cleanup still runs."""
    _patch_subprocess(monkeypatch)

    async def never_listens(host: str, port: int, **kwargs) -> bool:
        return False

    monkeypatch.setattr(service_mod, "_probe_port", never_listens)
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
        raise ServiceError(f"service never came up on {host}:{port}")

    return fast


def test_hosts_arg_formats_endpoints():
    eps = [
        ServiceEndpoint(host="h1", port=1611),
        ServiceEndpoint(host="h2", port=1612),
    ]
    assert hosts_arg(eps) == "h1:1611,h2:1612"
