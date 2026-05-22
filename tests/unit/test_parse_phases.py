"""Tests for results/parse.py phase classification and filtering.

Real-world CSV samples come from elbencho 3.1-1 against WEKA. The MKDIRS
case is a regression: previously this op was included as a phase with all
metric columns null, polluting the parsed Result.phases dict.
"""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

from elbencho_harness.config.models import ClientHost, PosixTarget, RunSpec, Workload
from elbencho_harness.engine.elbencho import ElbenchoVersion, artifacts_for
from elbencho_harness.results.parse import (
    NON_IO_OPERATIONS,
    _classify_phase,
    build_result,
)


def _spec() -> RunSpec:
    return RunSpec(
        run_id="01",
        spec_hash="sha256:x",
        target=PosixTarget(name="t", mount_path="/mnt", dataset_subdir="bench"),
        workload=Workload(
            name="w",
            block_size=1048576,
            rw_mix_pct_read=100,
            threads_per_client=8,
            io_depth=4,
            file_size=268435456,
            file_count=4,
            direct_io=True,
        ),
        clients=[ClientHost()],
    )


def test_classify_phase_handles_all_known_operations():
    assert _classify_phase("READ") == "read"
    assert _classify_phase("WRITE") == "write"
    assert _classify_phase("MKDIRS") == "mkdirs"
    assert _classify_phase("RMDIRS") == "rmdirs"
    assert _classify_phase("SYNC") == "sync"
    assert _classify_phase("DROPCACHE") == "drop_caches"
    assert _classify_phase("DROP_CACHES") == "drop_caches"
    assert _classify_phase("CLEANUP") == "cleanup"
    assert _classify_phase("STAT") == "stat"
    assert _classify_phase("DELETE") == "delete"
    assert _classify_phase("UNRECOGNIZED") == "UNRECOGNIZED"


def test_non_io_operations_set_is_correct():
    """The filter set must include every non-IO op classify can return."""
    assert {"mkdirs", "rmdirs", "sync", "drop_caches", "cleanup"} <= NON_IO_OPERATIONS


def test_elbencho_backend_filters_mkdirs_phase(tmp_path):
    """Regression: real WEKA run with --mkdirs added a MKDIRS row that landed
    in result.phases as a null-metric entry. Now skipped at backend.parse_results
    time."""
    from elbencho_harness.backends.elbencho import ElbenchoBackend

    # Trimmed real elbencho-3.1-1 header + 3 phase rows from a WEKA run.
    raw_dir = tmp_path
    csv = raw_dir / "run.csv"
    csv.write_text(
        "ISO date,label,path type,paths,hosts,threads,dirs,files,file size,"
        "block size,direct IO,random,random aligned,IO depth,shared paths,truncate,"
        "operation,time ms [first],time ms [last],entries/s [first],entries/s [last],"
        "IOPS [first],IOPS [last],MiB/s [first],MiB/s [last],CPU% [first],CPU% [last],"
        "entries [first],entries [last],MiB [first],MiB [last],"
        "Ent lat us [min],Ent lat us [avg],Ent lat us [max],"
        "IO lat us [min],IO lat us [avg],IO lat us [max]\n"
        "2026-05-22T16:09:38,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,"
        "MKDIRS,2,5,3277,1466,,,,,8,7,8,8,,,812,1068,1378,,,\n"
        "2026-05-22T16:09:38,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,"
        "WRITE,2134,2784,11,11,3494,2941,3494,2941,10,9,25,32,7460,8192,"
        "290635,609112,867164,1661,9383,120903\n"
        "2026-05-22T16:09:41,,dir,1,1,8,1,4,268435456,1048576,1,0,,4,1,0,"
        "READ,846,905,37,35,9673,9048,9673,9048,9,8,32,32,8192,8192,"
        "49483,214645,404799,553,3223,6922\n"
    )

    backend = ElbenchoBackend()
    phases, refs = backend.parse_results(raw_dir, command="elbencho ...")

    assert "mkdirs" not in phases
    assert "read" in phases
    assert "write" in phases
    assert phases["read"].throughput_mib_s_last == 9048
    assert phases["write"].throughput_mib_s_last == 2941
    assert refs.command == "elbencho ..."
    assert refs.csv_path == str(csv)
