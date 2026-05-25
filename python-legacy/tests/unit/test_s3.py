"""Tests for S3 target wiring: credentials resolution + extra build_argv flags."""

from __future__ import annotations

import pytest

from elbencho_harness.config.models import (
    ClientHost,
    RunSpec,
    S3Target,
    Workload,
)
from elbencho_harness.engine.coordinator import CoordinatorError, _inject_s3_credentials
from elbencho_harness.engine.elbencho import artifacts_for, build_argv


def _spec(workload: Workload) -> RunSpec:
    return RunSpec(
        run_id="01",
        spec_hash="sha256:x",
        target=S3Target(
            name="s3",
            endpoint="https://s3.example.com",
            bucket="bench-bucket",
            credentials_ref="env:S3_X",
            addressing="path",
        ),
        workload=workload,
        clients=[ClientHost()],
    )


# --- build_argv ---------------------------------------------------------------


def test_build_argv_s3_multipart_size_passed_through(tmp_path):
    spec = _spec(
        Workload(
            name="w",
            block_size=1048576,
            rw_mix_pct_read=0,
            file_size=4096,
            file_count=1,
            s3_multipart_size=8 * 1024 * 1024,
        )
    )
    argv, _ = build_argv(spec, artifacts_for(tmp_path))
    assert "--s3multipartsize" in argv
    assert str(8 * 1024 * 1024) in argv


def test_build_argv_s3_object_prefix_passed_through(tmp_path):
    spec = _spec(
        Workload(
            name="w",
            block_size=4096,
            rw_mix_pct_read=0,
            file_size=4096,
            file_count=1,
            s3_object_prefix="bench-2026-05/",
        )
    )
    argv, _ = build_argv(spec, artifacts_for(tmp_path))
    assert "--s3objectprefix" in argv
    assert "bench-2026-05/" in argv


def test_build_argv_omits_s3_knobs_when_unset(tmp_path):
    spec = _spec(
        Workload(name="w", block_size=4096, rw_mix_pct_read=0, file_size=4096, file_count=1)
    )
    argv, _ = build_argv(spec, artifacts_for(tmp_path))
    assert "--s3multipartsize" not in argv
    assert "--s3objectprefix" not in argv


# --- credentials resolution ---------------------------------------------------


def test_inject_credentials_env_with_colon_split():
    env: dict[str, str] = {"S3_X": "AKIAEXAMPLE:secretkey123"}
    _inject_s3_credentials(env, "env:S3_X")
    assert env["AWS_ACCESS_KEY_ID"] == "AKIAEXAMPLE"
    assert env["AWS_SECRET_ACCESS_KEY"] == "secretkey123"


def test_inject_credentials_env_already_aws_set_is_preserved():
    """If the caller already exported AWS_ACCESS_KEY_ID, don't clobber it."""
    env: dict[str, str] = {
        "S3_X": "ignored:ignored",
        "AWS_ACCESS_KEY_ID": "preset-access",
        "AWS_SECRET_ACCESS_KEY": "preset-secret",
    }
    _inject_s3_credentials(env, "env:S3_X")
    assert env["AWS_ACCESS_KEY_ID"] == "preset-access"
    assert env["AWS_SECRET_ACCESS_KEY"] == "preset-secret"


def test_inject_credentials_env_missing_var_raises():
    env: dict[str, str] = {}
    with pytest.raises(CoordinatorError, match="unset"):
        _inject_s3_credentials(env, "env:NOPE")


def test_inject_credentials_file_colon_format(tmp_path):
    creds = tmp_path / "creds"
    creds.write_text("AKIAFILE:secretfromfile\n")
    env: dict[str, str] = {}
    _inject_s3_credentials(env, f"file:{creds}")
    assert env["AWS_ACCESS_KEY_ID"] == "AKIAFILE"
    assert env["AWS_SECRET_ACCESS_KEY"] == "secretfromfile"


def test_inject_credentials_file_two_line_format(tmp_path):
    creds = tmp_path / "creds"
    creds.write_text("AKIATWOLINE\nsecrettwoline\n")
    env: dict[str, str] = {}
    _inject_s3_credentials(env, f"file:{creds}")
    assert env["AWS_ACCESS_KEY_ID"] == "AKIATWOLINE"
    assert env["AWS_SECRET_ACCESS_KEY"] == "secrettwoline"


def test_inject_credentials_file_not_found_raises(tmp_path):
    env: dict[str, str] = {}
    with pytest.raises(CoordinatorError, match="not found"):
        _inject_s3_credentials(env, f"file:{tmp_path}/nope")


def test_inject_credentials_unknown_scheme_raises():
    env: dict[str, str] = {}
    with pytest.raises(CoordinatorError, match="unsupported"):
        _inject_s3_credentials(env, "inline:AKIA:secret")


def test_s3_target_rejects_inline_credentials():
    """The model itself blocks credentials_ref strings that aren't env: or file:."""
    with pytest.raises(Exception):  # pydantic raises ValidationError
        S3Target(
            name="s3",
            endpoint="https://s3.example.com",
            bucket="b",
            credentials_ref="AKIA:inline",
        )
