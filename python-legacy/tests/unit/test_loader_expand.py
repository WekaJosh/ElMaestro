"""Tests for the YAML loader's placeholder expansion.

These cover the portability fix: a config that references a sibling file (or
$HOME, or an env var) should load identically regardless of where the repo
lives on disk or which machine runs it.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from elbencho_harness.config.loader import _expand_placeholders, load_run_plan


def test_config_dir_resolves_to_yaml_parent(tmp_path):
    raw = "elbencho_path: ${CONFIG_DIR}/fake.sh"
    out = _expand_placeholders(raw, config_dir=tmp_path / "subdir")
    assert out == f"elbencho_path: {tmp_path}/subdir/fake.sh"


def test_home_placeholder_resolves_to_user_home(tmp_path):
    raw = "ssh_key: ${HOME}/.ssh/id_ed25519"
    out = _expand_placeholders(raw, config_dir=tmp_path)
    assert out == f"ssh_key: {Path.home()}/.ssh/id_ed25519"


def test_env_placeholder_resolves_from_environment(monkeypatch, tmp_path):
    monkeypatch.setenv("MY_BENCH_BUCKET", "the-bucket")
    raw = "bucket: $ENV{MY_BENCH_BUCKET}"
    out = _expand_placeholders(raw, config_dir=tmp_path)
    assert out == "bucket: the-bucket"


def test_env_placeholder_missing_var_becomes_empty(tmp_path):
    raw = "bucket: $ENV{DEFINITELY_NOT_SET_12345}"
    out = _expand_placeholders(raw, config_dir=tmp_path)
    assert out == "bucket: "


def test_smoke_fixture_loads_from_any_cwd(tmp_path, monkeypatch):
    """The shipped sweep_smoke fixture must work regardless of cwd."""
    fixture_path = Path(__file__).resolve().parents[1] / "fixtures" / "sweep_smoke.yaml"
    monkeypatch.chdir(tmp_path)  # simulate running from an unrelated dir
    plan = load_run_plan(fixture_path)
    # The expanded path must be absolute and end with the fixture filename.
    fake_path = plan.clients[0].elbencho_path
    assert os.path.isabs(fake_path)
    assert fake_path.endswith("fake_elbencho.sh")
    assert Path(fake_path).is_file()


def test_smoke_fixture_loader_does_not_leak_my_machine_path(tmp_path):
    """Regression: the fixture used to hardcode /Users/josh.h/.../fixtures.
    A fresh clone on any other machine must not see that string at all."""
    fixture_path = Path(__file__).resolve().parents[1] / "fixtures" / "sweep_smoke.yaml"
    raw_text = fixture_path.read_text()
    assert "/Users/" not in raw_text
    assert "/home/" not in raw_text
    assert "${CONFIG_DIR}" in raw_text
