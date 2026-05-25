"""Tests for engine/ssh.py.

The SSH layer drives subprocess(ssh) since v0.9. These tests monkeypatch
subprocess.run so we never actually shell out.
"""

from __future__ import annotations

import asyncio
import shlex
import subprocess
from dataclasses import dataclass, field

import pytest

from elbencho_harness.config.models import ClientHost
from elbencho_harness.engine import ssh as ssh_mod
from elbencho_harness.engine.ssh import (
    SSHError,
    _ssh_base_argv,
    open_runner,
    open_runners,
)


# --- Fakes --------------------------------------------------------------------


@dataclass
class FakeCompletedProcess:
    returncode: int = 0
    stdout: str = ""
    stderr: str = ""


@dataclass
class _Call:
    """One captured subprocess.run invocation."""

    argv: list[str]
    timeout: float | None


def _captured_runner(monkeypatch, returncode: int = 0, stdout: str = "", stderr: str = ""):
    """Replace subprocess.run with a fake that records argv + timeout per call."""
    calls: list[_Call] = []

    def fake_run(argv, **kwargs):
        calls.append(_Call(argv=list(argv), timeout=kwargs.get("timeout")))
        return FakeCompletedProcess(returncode=returncode, stdout=stdout, stderr=stderr)

    monkeypatch.setattr(ssh_mod.subprocess, "run", fake_run)
    return calls


# --- _ssh_base_argv -----------------------------------------------------------


def test_ssh_base_argv_minimal():
    argv = _ssh_base_argv(ClientHost(host="h"))
    assert argv[0] == "ssh"
    assert "BatchMode=yes" in argv
    assert "ConnectTimeout=10" in argv
    assert argv[-1] == "h"  # no user prefix when ssh_user is None
    assert "-p" not in argv  # default port 22 -> no -p flag


def test_ssh_base_argv_with_user_port_key(tmp_path):
    key = tmp_path / "id_ed25519"
    key.write_text("fake")
    client = ClientHost(host="h", ssh_user="bench", ssh_port=2222, ssh_key=key)
    argv = _ssh_base_argv(client)
    assert "-p" in argv
    assert "2222" in argv
    assert "-i" in argv
    assert str(key) in argv
    assert "bench@h" in argv


def test_ssh_base_argv_expands_home_in_key_path(monkeypatch):
    """`ssh_key: ~/.ssh/foo` must expand before reaching ssh."""
    monkeypatch.setenv("HOME", "/tmp/fakehome")
    client = ClientHost(host="h", ssh_user="u", ssh_key="~/.ssh/id_ed25519")
    argv = _ssh_base_argv(client)
    # The path passed to -i must be absolute, not start with ~.
    i_idx = argv.index("-i")
    key_arg = argv[i_idx + 1]
    assert not key_arg.startswith("~")


# --- SSHRunner.run ------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_captures_argv_and_returns_result(monkeypatch):
    calls = _captured_runner(monkeypatch, stdout="hello\n")
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        result = await runner.run(["echo", "hello"])
    assert result.ok is True
    assert result.stdout == "hello\n"
    # First call was the health check (`true`); second is our echo.
    assert any("echo hello" in c.argv[-1] for c in calls)


@pytest.mark.asyncio
async def test_run_shell_quotes_argv(monkeypatch):
    """Argv list elements with spaces must be quoted before sending over ssh."""
    calls = _captured_runner(monkeypatch)
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        await runner.run(["echo", "hello world", "$SECRET"])
    # The final arg of the ssh argv is the shell-quoted remote command.
    last_remote_cmd = [c.argv[-1] for c in calls if "echo" in c.argv[-1]][0]
    # Both special-character args must be safely quoted.
    assert "'hello world'" in last_remote_cmd
    assert "'$SECRET'" in last_remote_cmd


@pytest.mark.asyncio
async def test_run_timeout_propagates_as_ssh_error(monkeypatch):
    def fake_run(argv, **kwargs):
        raise subprocess.TimeoutExpired(cmd=argv, timeout=kwargs.get("timeout", 0))

    monkeypatch.setattr(ssh_mod.subprocess, "run", fake_run)
    with pytest.raises(SSHError, match="timeout"):
        async with open_runner(ClientHost(host="h", ssh_user="u")):
            pass


@pytest.mark.asyncio
async def test_open_runner_health_check_failure_raises(monkeypatch):
    """If the initial `true` returns non-zero, open_runner must raise."""
    _captured_runner(monkeypatch, returncode=255, stderr="permission denied")
    with pytest.raises(SSHError, match="ssh to dead failed"):
        async with open_runner(ClientHost(host="dead", ssh_user="u")):
            pass


# --- SSHRunner.start_background / stop_background -----------------------------


@pytest.mark.asyncio
async def test_start_background_wraps_with_nohup_and_writes_pidfile(monkeypatch):
    calls = _captured_runner(monkeypatch)
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        bg = await runner.start_background(
            ["elbencho", "--service", "--port", "1611"]
        )
    # The remote command should be wrapped in nohup ... & with a PID file.
    wrapper = [c.argv[-1] for c in calls if "nohup" in c.argv[-1]][0]
    assert "nohup elbencho --service --port 1611" in wrapper
    assert "echo $!" in wrapper
    assert bg.pid_file.startswith("/tmp/elmaestro-")
    assert bg.pid_file.endswith(".pid")
    assert bg.log_file.startswith("/tmp/elmaestro-")
    assert bg.log_file.endswith(".log")


@pytest.mark.asyncio
async def test_stop_background_kills_each_started_process(monkeypatch):
    calls = _captured_runner(monkeypatch)
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        bg1 = await runner.start_background(["svc1"])
        bg2 = await runner.start_background(["svc2"])
    # At runner.close(), every PID file must be referenced in a kill cmd.
    cleanup_cmds = [c.argv[-1] for c in calls if "kill" in c.argv[-1]]
    assert any(bg1.pid_file in c for c in cleanup_cmds)
    assert any(bg2.pid_file in c for c in cleanup_cmds)


@pytest.mark.asyncio
async def test_stop_background_tolerates_dead_remote(monkeypatch):
    """If the cleanup ssh call itself fails, teardown must not propagate."""
    state = {"started": False}

    def fake_run(argv, **kwargs):
        cmd = argv[-1] if argv else ""
        # First call is health check `true`; pass.
        # Second is the start_background nohup; pass.
        # Third is the stop_background cleanup; simulate dead host.
        if "kill" in cmd:
            raise OSError("connection refused")
        return FakeCompletedProcess()

    monkeypatch.setattr(ssh_mod.subprocess, "run", fake_run)
    # Should NOT raise.
    async with open_runner(ClientHost(host="h", ssh_user="u")) as runner:
        await runner.start_background(["x"])


# --- open_runners (concurrent) ------------------------------------------------


@pytest.mark.asyncio
async def test_open_runners_health_checks_each_client(monkeypatch):
    calls = _captured_runner(monkeypatch)
    clients = [
        ClientHost(host="h1", ssh_user="u"),
        ClientHost(host="h2", ssh_user="u"),
        ClientHost(host="h3", ssh_user="u"),
    ]
    async with open_runners(clients) as runners:
        assert [r.host for r in runners] == ["h1", "h2", "h3"]
    # Three `true` health checks must have happened (one per client).
    true_calls = [c for c in calls if c.argv[-1] == "true"]
    assert len(true_calls) == 3
