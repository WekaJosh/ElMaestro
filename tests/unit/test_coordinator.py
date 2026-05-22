"""Tests for engine/coordinator.py dispatch.

Verifies the local vs. fan-out split, plus build_argv with --hosts.
"""

from __future__ import annotations

from elbencho_harness.config.models import ClientHost, PosixTarget, RunSpec, Workload
from elbencho_harness.engine import coordinator as coord_mod
from elbencho_harness.engine.coordinator import _is_localhost_only
from elbencho_harness.engine.elbencho import artifacts_for, build_argv


def _spec(clients: list[ClientHost]) -> RunSpec:
    return RunSpec(
        run_id="01",
        spec_hash="sha256:x",
        target=PosixTarget(name="t", mount_path="/mnt", dataset_subdir="bench"),
        workload=Workload(
            name="w", block_size=4096, rw_mix_pct_read=100, file_size=4096, file_count=1
        ),
        clients=clients,
    )


def test_is_localhost_only_true_for_single_localhost():
    assert _is_localhost_only([ClientHost(host="localhost")]) is True
    assert _is_localhost_only([ClientHost(host="127.0.0.1")]) is True


def test_is_localhost_only_false_for_remote():
    assert _is_localhost_only([ClientHost(host="worker-01")]) is False


def test_is_localhost_only_false_for_multiple_clients():
    assert _is_localhost_only([ClientHost(host="localhost"), ClientHost(host="h2")]) is False


def test_run_dispatches_to_local_for_single_localhost(monkeypatch, tmp_path):
    called: dict[str, bool] = {"local": False, "fanout": False}

    def fake_local(spec, **kwargs):
        called["local"] = True
        return "local-result"

    def fake_fanout(spec, **kwargs):
        called["fanout"] = True
        return "fanout-result"

    monkeypatch.setattr(coord_mod, "run_locally", fake_local)
    monkeypatch.setattr(coord_mod, "run_fanout", fake_fanout)

    result = coord_mod.run(_spec([ClientHost(host="localhost")]), spec_dir=tmp_path)
    assert result == "local-result"
    assert called == {"local": True, "fanout": False}


def test_run_dispatches_to_fanout_for_remote(monkeypatch, tmp_path):
    called: dict[str, bool] = {"local": False, "fanout": False}

    def fake_local(spec, **kwargs):
        called["local"] = True
        return "local"

    def fake_fanout(spec, **kwargs):
        called["fanout"] = True
        return "fanout"

    monkeypatch.setattr(coord_mod, "run_locally", fake_local)
    monkeypatch.setattr(coord_mod, "run_fanout", fake_fanout)

    result = coord_mod.run(
        _spec([ClientHost(host="h1", ssh_user="u"), ClientHost(host="h2", ssh_user="u")]),
        spec_dir=tmp_path,
    )
    assert result == "fanout"
    assert called == {"local": False, "fanout": True}


def test_build_argv_includes_hosts_when_provided(tmp_path):
    spec = _spec([ClientHost(host="h1", ssh_user="u")])
    argv, _ = build_argv(spec, artifacts_for(tmp_path), hosts="h1:1611,h2:1611")
    assert "--hosts" in argv
    assert "h1:1611,h2:1611" in argv


def test_build_argv_omits_hosts_when_not_provided(tmp_path):
    spec = _spec([ClientHost()])
    argv, _ = build_argv(spec, artifacts_for(tmp_path))
    assert "--hosts" not in argv
