"""Tests for the fio backend: command construction, JSON parsing, target gating."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from elbencho_harness.backends.fio import (
    FioBackend,
    _fio_rw,
    _primary_phase_for,
)
from elbencho_harness.config.models import (
    ClientHost,
    PosixTarget,
    RunSpec,
    S3Target,
    Workload,
)


def _spec(workload: Workload, target=None) -> RunSpec:
    return RunSpec(
        run_id="01",
        spec_hash="sha256:x",
        target=target or PosixTarget(name="t", mount_path="/mnt", dataset_subdir="bench"),
        workload=workload,
        clients=[ClientHost()],
    )


def _wl(**kwargs) -> Workload:
    defaults = dict(
        name="w",
        block_size=1048576,
        rw_mix_pct_read=100,
        threads_per_client=4,
        io_depth=4,
        file_size=4096,
        file_count=1,
        direct_io=True,
    )
    defaults.update(kwargs)
    return Workload(**defaults)


# --- rw mapping ---------------------------------------------------------------


@pytest.mark.parametrize(
    "pattern,mix,expected",
    [
        ("seq", 100, "read"),
        ("seq", 0, "write"),
        ("seq", 70, "rw"),
        ("rand", 100, "randread"),
        ("rand", 0, "randwrite"),
        ("rand", 30, "randrw"),
    ],
)
def test_fio_rw_mapping(pattern, mix, expected):
    assert _fio_rw(pattern, mix) == expected


def test_primary_phase_for_pure_read():
    assert _primary_phase_for(100) == "read"


def test_primary_phase_for_pure_write():
    assert _primary_phase_for(0) == "write"


def test_primary_phase_for_mixed():
    assert _primary_phase_for(70) == "mixed"


# --- build_argv ---------------------------------------------------------------


def test_build_argv_writes_job_file_with_required_keys(tmp_path):
    backend = FioBackend()
    argv, primary = backend.build_argv(_spec(_wl()), tmp_path, local_path="/usr/bin/fio")
    job_file = tmp_path / "job.fio"
    assert job_file.is_file()
    body = job_file.read_text()
    assert "[w]" in body
    assert "directory=/mnt/bench" in body
    assert "rw=read" in body
    assert "bs=1048576" in body
    assert "iodepth=4" in body
    assert "numjobs=4" in body
    assert "direct=1" in body
    assert "group_reporting=1" in body
    assert primary == "read"


def test_build_argv_appends_job_file_at_end(tmp_path):
    backend = FioBackend()
    argv, _ = backend.build_argv(_spec(_wl()), tmp_path, local_path="/usr/bin/fio")
    assert argv[0] == "/usr/bin/fio"
    assert argv[-1] == str(tmp_path / "job.fio")
    assert any(a.startswith("--output=") for a in argv)
    assert "--output-format=json" in argv


def test_build_argv_emits_rwmixread_for_mixed_workload(tmp_path):
    backend = FioBackend()
    argv, primary = backend.build_argv(
        _spec(_wl(rw_mix_pct_read=70)), tmp_path, local_path="/usr/bin/fio"
    )
    job_body = (tmp_path / "job.fio").read_text()
    assert "rwmixread=70" in job_body
    assert "rw=rw" in job_body
    assert primary == "mixed"


def test_build_argv_translates_host_port_to_fio_comma_format(tmp_path):
    """Regression: fio's --client wants 'host,port', not 'host:port' like
    elbencho's --hosts. The first live fan-out run failed because we passed
    the elbencho form through unchanged."""
    backend = FioBackend()
    argv, _ = backend.build_argv(
        _spec(_wl()), tmp_path, local_path="/usr/bin/fio", hosts="h1:8765,h2:8765"
    )
    assert "--client=h1,8765" in argv
    assert "--client=h2,8765" in argv
    # Don't leak the elbencho format.
    assert "--client=h1:8765" not in argv


def test_build_argv_host_without_port_passes_through_bare(tmp_path):
    backend = FioBackend()
    argv, _ = backend.build_argv(
        _spec(_wl()), tmp_path, local_path="/usr/bin/fio", hosts="h1,h2"
    )
    # 'h1' has no port, just emit --client=h1.
    assert "--client=h1" in argv
    assert "--client=h2" in argv


def test_build_argv_appends_extra_flags_to_job_file(tmp_path):
    backend = FioBackend()
    backend.build_argv(
        _spec(_wl(extra_flags=["ioengine=libaio", "buffered=0"])),
        tmp_path,
        local_path="/usr/bin/fio",
    )
    body = (tmp_path / "job.fio").read_text()
    assert "ioengine=libaio" in body
    assert "buffered=0" in body


def test_build_argv_rejects_s3_target(tmp_path):
    backend = FioBackend()
    spec = _spec(
        _wl(),
        target=S3Target(
            name="s3", endpoint="https://s3", bucket="b", credentials_ref="env:X"
        ),
    )
    with pytest.raises(ValueError, match="POSIX"):
        backend.build_argv(spec, tmp_path, local_path="/usr/bin/fio")


# --- parse_results ------------------------------------------------------------


_SAMPLE_JSON = {
    "fio version": "fio-3.36",
    "jobs": [
        {
            "jobname": "bench",
            "read": {
                "io_bytes": 1073741824,
                "bw": 9216000,
                "iops": 9000,
                "clat_ns": {
                    "min": 12000,
                    "max": 7800000,
                    "mean": 612000,
                    "percentile": {
                        "50.000000": 480000,
                        "99.000000": 1500000,
                        "99.900000": 3100000,
                    },
                },
            },
            "write": {
                "io_bytes": 0,
                "bw": 0,
                "iops": 0,
                "clat_ns": {"min": 0, "max": 0, "mean": 0},
            },
        }
    ],
}


def test_parse_results_extracts_read_phase(tmp_path):
    (tmp_path / "run.json").write_text(json.dumps(_SAMPLE_JSON))
    backend = FioBackend()
    phases, refs = backend.parse_results(tmp_path, command="fio ...")
    assert "read" in phases
    rd = phases["read"]
    # bw=9216000 KiB/s -> ~9000 MiB/s
    assert rd.throughput_mib_s_last == pytest.approx(9000.0, rel=0.01)
    assert rd.iops_last == 9000
    # clat mean = 612000 ns -> 612 us
    assert rd.io_lat_us.avg == pytest.approx(612.0)
    assert rd.io_lat_us.min == pytest.approx(12.0)
    assert rd.io_lat_us.max == pytest.approx(7800.0)


def test_parse_results_skips_empty_write_phase_in_pure_read(tmp_path):
    (tmp_path / "run.json").write_text(json.dumps(_SAMPLE_JSON))
    backend = FioBackend()
    phases, _ = backend.parse_results(tmp_path, command="fio ...")
    # write section has zero io_bytes + zero iops, so it must be skipped.
    assert "write" not in phases


def test_parse_results_extracts_percentiles(tmp_path):
    (tmp_path / "run.json").write_text(json.dumps(_SAMPLE_JSON))
    backend = FioBackend()
    phases, _ = backend.parse_results(tmp_path, command="fio ...")
    pcts = phases["read"].latency_percentiles_us
    assert pcts.get("p50") == pytest.approx(480.0)
    assert pcts.get("p99") == pytest.approx(1500.0)
    assert pcts.get("p99.9") == pytest.approx(3100.0)


def test_parse_results_returns_artifact_refs(tmp_path):
    (tmp_path / "run.json").write_text(json.dumps(_SAMPLE_JSON))
    backend = FioBackend()
    _, refs = backend.parse_results(tmp_path, command="fio --xyz")
    assert refs.command == "fio --xyz"
    assert refs.jsonfile_path == str(tmp_path / "run.json")
    assert refs.csv_path is None  # fio doesn't emit CSV


def test_parse_results_handles_missing_json_gracefully(tmp_path):
    # No run.json file at all.
    backend = FioBackend()
    phases, refs = backend.parse_results(tmp_path, command="fio ...")
    assert phases == {}
    assert refs.jsonfile_path.endswith("run.json")


def test_parse_results_strips_fio_client_server_preamble(tmp_path):
    """Regression: real fio 3.19 multi-client runs prefix the JSON document
    with host-prefixed status lines like '<weka54> Starting 8 processes'.
    The parser must locate the JSON and skip past the preamble.
    """
    body = (
        "<weka54> seq-read-base: (g=0): rw=read, bs=(R) 64KiB-64KiB\n"
        "<weka54> ...\n"
        "<weka54> Starting 8 processes\n"
        "<weka54> seq-read-base: \n"
        + json.dumps(_SAMPLE_JSON)
    )
    (tmp_path / "run.json").write_text(body)
    backend = FioBackend()
    phases, _ = backend.parse_results(tmp_path, command="fio ...")
    assert "read" in phases
    assert phases["read"].iops_last == 9000


def test_parse_results_picks_all_clients_aggregate_from_client_stats(tmp_path):
    """In multi-client mode fio emits per-client entries plus an aggregate
    'All clients' entry. The parser must use the aggregate."""
    data = {
        "fio version": "fio-3.36",
        "client_stats": [
            {
                "hostname": "host1",
                "read": {
                    "io_bytes": 500_000_000,
                    "bw": 4_000_000,
                    "iops": 4000,
                    "clat_ns": {"min": 100, "max": 200, "mean": 150},
                },
            },
            {
                "hostname": "host2",
                "read": {
                    "io_bytes": 500_000_000,
                    "bw": 5_000_000,
                    "iops": 5000,
                    "clat_ns": {"min": 100, "max": 200, "mean": 150},
                },
            },
            {
                "hostname": "All clients",
                "read": {
                    "io_bytes": 1_000_000_000,
                    "bw": 9_000_000,
                    "iops": 9000,
                    "clat_ns": {"min": 100, "max": 200, "mean": 150},
                },
            },
        ],
    }
    (tmp_path / "run.json").write_text(json.dumps(data))
    backend = FioBackend()
    phases, _ = backend.parse_results(tmp_path, command="fio ...")
    # Aggregate iops, not single client's.
    assert phases["read"].iops_last == 9000


# --- supports_target ----------------------------------------------------------


def test_supports_posix_target():
    backend = FioBackend()
    posix = PosixTarget(name="t", mount_path="/mnt")
    assert backend.supports_target(posix).supported is True


def test_does_not_support_s3_target_yet():
    backend = FioBackend()
    s3 = S3Target(name="s3", endpoint="https://s3", bucket="b", credentials_ref="env:X")
    sup = backend.supports_target(s3)
    assert sup.supported is False
    assert "S3" in sup.reason


# --- service_command ----------------------------------------------------------


def test_service_command_uses_fio_server_bind():
    backend = FioBackend()
    cmd = backend.service_command(
        ClientHost(host="h", ssh_user="u", elbencho_path="/usr/bin/fio", service_port=8765)
    )
    assert cmd == ["/usr/bin/fio", "--server=,N:8765"]
