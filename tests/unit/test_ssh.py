"""Tests for engine/ssh.py.

These never reach a real SSH daemon. We monkeypatch `asyncssh.connect` to
return a fake connection that records what would have happened.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from typing import Any

import pytest

from elbencho_harness.config.models import ClientHost
from elbencho_harness.engine import ssh as ssh_mod
from elbencho_harness.engine.ssh import SSHError, open_runner, open_runners


# --- Fakes --------------------------------------------------------------------


@dataclass
class FakeCompletedProcess:
    exit_status: int = 0
    stdout: str = ""
    stderr: str = ""


class FakeBgProc:
    def __init__(self) -> None:
        self.terminated = False
        self.closed = False

    def terminate(self) -> None:
        self.terminated = True

    async def wait_closed(self) -> None:
        self.closed = True


@dataclass
class FakeConn:
    host: str
    port: int
    username: str | None = None
    closed: bool = False
    commands_run: list[str] = field(default_factory=list)
    bg_commands: list[str] = field(default_factory=list)
    bg_procs: list[FakeBgProc] = field(default_factory=list)
    # configurable behavior
    run_result: FakeCompletedProcess = field(default_factory=FakeCompletedProcess)
    raise_on_run: Exception | None = None

    async def run(self, cmd: str, check: bool = False) -> FakeCompletedProcess:
        if self.raise_on_run is not None:
            raise self.raise_on_run
        self.commands_run.append(cmd)
        return self.run_result

    async def create_process(self, cmd: str) -> FakeBgProc:
        self.bg_commands.append(cmd)
        p = FakeBgProc()
        self.bg_procs.append(p)
        return p

    def close(self) -> None:
        self.closed = True

    async def wait_closed(self) -> None:
        # asyncssh's API has this as awaitable; tests rely on close+wait being safe
        pass


@pytest.fixture
def mock_asyncssh(monkeypatch):
    """Replace asyncssh.connect with a factory that returns FakeConn."""
    conns: list[FakeConn] = []

    async def fake_connect(**kwargs: Any) -> FakeConn:
        c = FakeConn(
            host=kwargs.get("host", ""),
            port=kwargs.get("port", 22),
            username=kwargs.get("username"),
        )
        conns.append(c)
        return c

    monkeypatch.setattr(ssh_mod.asyncssh, "connect", fake_connect)
    return conns


# --- Tests --------------------------------------------------------------------


@pytest.mark.asyncio
async def test_open_runner_yields_connected_runner_and_closes(mock_asyncssh):
    client = ClientHost(host="worker-01", ssh_user="bench", ssh_port=22)
    async with open_runner(client) as runner:
        assert runner.host == "worker-01"
        assert len(mock_asyncssh) == 1
        assert mock_asyncssh[0].host == "worker-01"
        assert mock_asyncssh[0].username == "bench"
    assert mock_asyncssh[0].closed is True


@pytest.mark.asyncio
async def test_run_one_shot_records_command_and_returns_result(mock_asyncssh):
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        mock_asyncssh[0].run_result = FakeCompletedProcess(
            exit_status=0, stdout="elbencho version 3.1.3\n"
        )
        result = await runner.run(["elbencho", "--version"])
    assert result.ok is True
    assert "elbencho version 3.1.3" in result.stdout
    # Must have shell-quoted the argv into a string
    assert mock_asyncssh[0].commands_run == ["elbencho --version"]


@pytest.mark.asyncio
async def test_start_background_records_and_closes_on_exit(mock_asyncssh):
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        await runner.start_background(["elbencho", "--service", "--port", "1611"])
        assert mock_asyncssh[0].bg_commands == ["elbencho --service --port 1611"]
        assert len(mock_asyncssh[0].bg_procs) == 1
    # Once the context exits, the bg proc should be terminated.
    assert mock_asyncssh[0].bg_procs[0].terminated is True


@pytest.mark.asyncio
async def test_open_runners_connects_in_parallel(mock_asyncssh):
    clients = [
        ClientHost(host="h1", ssh_user="u"),
        ClientHost(host="h2", ssh_user="u"),
        ClientHost(host="h3", ssh_user="u"),
    ]
    async with open_runners(clients) as runners:
        assert [r.host for r in runners] == ["h1", "h2", "h3"]
        assert len(mock_asyncssh) == 3
    for c in mock_asyncssh:
        assert c.closed is True


@pytest.mark.asyncio
async def test_open_runner_raises_ssh_error_on_connect_failure(monkeypatch):
    async def boom(**kwargs):
        raise OSError("connection refused")

    monkeypatch.setattr(ssh_mod.asyncssh, "connect", boom)
    with pytest.raises(SSHError, match="failed to connect"):
        async with open_runner(ClientHost(host="dead", ssh_user="u")):
            pass


@pytest.mark.asyncio
async def test_open_runner_timeout_propagates_as_ssh_error(monkeypatch):
    async def slow(**kwargs):
        await asyncio.sleep(10)
        return None

    monkeypatch.setattr(ssh_mod.asyncssh, "connect", slow)
    with pytest.raises(SSHError, match="timeout"):
        async with open_runner(ClientHost(host="slow", ssh_user="u"), connect_timeout=0.1):
            pass


def test_connect_kwargs_uses_explicit_key_when_set(tmp_path):
    keyfile = tmp_path / "id_ed25519"
    keyfile.write_text("fake")
    client = ClientHost(host="h", ssh_user="u", ssh_key=keyfile)
    kw = ssh_mod._connect_kwargs(client)
    assert kw["client_keys"] == [str(keyfile)]


def test_connect_kwargs_drops_client_keys_when_none():
    client = ClientHost(host="h", ssh_user="u")
    kw = ssh_mod._connect_kwargs(client)
    assert "client_keys" not in kw
