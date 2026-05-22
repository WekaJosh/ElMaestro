"""elbencho integration: flag builder, version detection, CSV/JSON parser.

This is the ONLY module that knows elbencho's flag names and output schema.
Everything else talks to it through RunSpec in and parsed dict out.
"""

from __future__ import annotations

import csv
import json
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..config.models import PosixTarget, RunSpec, S3Target


@dataclass
class ElbenchoArtifacts:
    """Filesystem paths where elbencho writes its output for one RunSpec."""

    csv: Path
    jsonfile: Path
    resfile: Path
    stdout: Path
    livecsv: Path  # populated when --livecsv is requested (v0.5+ TUI)


@dataclass
class ElbenchoVersion:
    raw: str
    version: str | None
    features: list[str] = field(default_factory=list)

    def has(self, feature: str) -> bool:
        return any(feature.lower() == f.lower() for f in self.features)


def detect_version(elbencho_path: str = "elbencho") -> ElbenchoVersion:
    """Run `<elbencho> --version` and parse output.

    elbencho emits a multi-line version string that includes enabled feature flags
    like 'S3', 'CUDA', 'CUFILE'. We pull the version number from the first matching
    line and any feature tokens from the rest.
    """
    proc = subprocess.run(
        [elbencho_path, "--version"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    raw = (proc.stdout or "") + (proc.stderr or "")
    version: str | None = None
    features: list[str] = []
    m = re.search(r"version[:\s]+v?(\d+\.\d+(?:\.\d+)?[^\s]*)", raw, re.IGNORECASE)
    if m:
        version = m.group(1)
    # Feature tokens we care about.
    for feat in ("S3", "CUDA", "CUFILE"):
        if re.search(rf"\b{feat}\b", raw):
            features.append(feat)
    return ElbenchoVersion(raw=raw.strip(), version=version, features=features)


def artifacts_for(output_dir: Path) -> ElbenchoArtifacts:
    output_dir.mkdir(parents=True, exist_ok=True)
    return ElbenchoArtifacts(
        csv=output_dir / "run.csv",
        jsonfile=output_dir / "run.json",
        resfile=output_dir / "run.txt",
        stdout=output_dir / "stdout.log",
        livecsv=output_dir / "live.csv",
    )


def _phases_for_mix(rw_mix_pct_read: int) -> tuple[list[str], str]:
    """Translate rw_mix_pct_read into elbencho phase flags + primary phase label.

    100 -> create+read (`-w -r`), primary='read'
      0 -> write only  (`-w`),    primary='write'
    1-99 -> mixed     (`-w --rwmixpct N`), primary='mixed'
    """
    if rw_mix_pct_read == 100:
        return (["-w", "-r"], "read")
    if rw_mix_pct_read == 0:
        return (["-w"], "write")
    return (["-w", "--rwmixpct", str(rw_mix_pct_read)], "mixed")


def build_argv(
    run_spec: RunSpec,
    artifacts: ElbenchoArtifacts,
    *,
    elbencho_path: str = "elbencho",
    include_livecsv: bool = False,
    live_interval_ms: int = 1000,
    hosts: str | None = None,
) -> tuple[list[str], str]:
    """Build the elbencho command line for a RunSpec.

    Returns (argv, primary_phase). primary_phase is the phase whose numbers are
    the headline in the report (e.g. 'read' for a 100/0 workload that was
    populated by an implicit write phase).

    `hosts`, when provided, is passed as elbencho's --hosts flag for multi-client
    coordination (e.g. "host1:1611,host2:1611"). The coordinator constructs this
    after starting service-mode elbencho on each remote host.
    """
    wl = run_spec.workload
    argv: list[str] = [elbencho_path]

    # Always-on structured output.
    argv += ["--csvfile", str(artifacts.csv)]
    argv += ["--jsonfile", str(artifacts.jsonfile)]
    argv += ["--resfile", str(artifacts.resfile)]
    argv += ["--latpercent", "--latpercent9s", "3"]

    if include_livecsv:
        argv += ["--livecsv", str(artifacts.livecsv), "--liveint", str(live_interval_ms)]

    # Multi-client fan-out.
    if hosts:
        argv += ["--hosts", hosts]

    # Workload basics.
    argv += ["-b", str(wl.block_size)]
    argv += ["-t", str(wl.threads_per_client)]
    if wl.io_depth > 1:
        argv += ["--iodepth", str(wl.io_depth)]
    if wl.pattern == "rand":
        argv += ["--rand"]

    # POSIX-only knobs.
    if isinstance(run_spec.target, PosixTarget):
        if wl.direct_io:
            argv += ["--direct"]
        if wl.drop_caches_before:
            argv += ["--dropcache"]
        if wl.sync_after_write:
            argv += ["--sync"]

    # Dataset sizing.
    if wl.file_size is not None:
        argv += ["-s", str(wl.file_size)]
    if wl.file_count is not None:
        argv += ["-N", str(wl.file_count)]
    if wl.duration_s is not None:
        argv += ["--timelimit", str(wl.duration_s)]

    # Operation phases.
    phase_flags, primary = _phases_for_mix(wl.rw_mix_pct_read)
    argv += phase_flags

    # S3 vs POSIX positional / endpoint args.
    tgt = run_spec.target
    if isinstance(tgt, PosixTarget):
        argv += ["--mkdirs"]  # ensure dataset dir exists; safe if it already does
        dataset_path = tgt.mount_path / tgt.dataset_subdir
        argv.append(str(dataset_path))
    elif isinstance(tgt, S3Target):
        argv += ["--s3endpoints", tgt.endpoint]
        if tgt.region:
            argv += ["--s3region", tgt.region]
        if tgt.addressing == "virtual":
            argv += ["--s3virtaddr"]
        argv.append(tgt.bucket)

    # User escape hatch (must come last so it can override).
    argv += list(wl.extra_flags)

    return argv, primary


# ---------------------------------------------------------------------------
# CSV / JSON parsing
# ---------------------------------------------------------------------------


def _coerce_number(s: str) -> float | int | None:
    s = (s or "").strip()
    if not s or s.lower() in {"n/a", "na", "-"}:
        return None
    try:
        if "." in s or "e" in s.lower():
            return float(s)
        return int(s)
    except ValueError:
        return None


def _find_col(headers: list[str], *needles: str) -> str | None:
    """Find first header whose lower-cased text contains ALL needles."""
    needles_l = [n.lower() for n in needles]
    for h in headers:
        hl = h.lower()
        if all(n in hl for n in needles_l):
            return h
    return None


@dataclass
class PhaseRow:
    operation: str  # 'WRITE' / 'READ' / etc. as elbencho emits
    metrics: dict[str, float | int | None]
    raw: dict[str, str]  # all columns, untouched


def parse_csv(path: Path) -> list[PhaseRow]:
    """Parse elbencho --csvfile output into per-phase rows.

    elbencho appends to the CSV (one row per phase). We use csv.DictReader and
    pull metrics by header substring match so we tolerate small schema drift
    across versions.
    """
    if not path.is_file():
        return []
    with path.open("r", newline="", encoding="utf-8") as fh:
        reader = csv.DictReader(fh)
        headers = list(reader.fieldnames or [])
        rows = list(reader)

    op_col = _find_col(headers, "operation") or _find_col(headers, "op")

    # Metric column resolution. elbencho emits paired [first]/[last] columns for
    # most metrics; we pull both for use in the result schema's First/Last Done.
    def col(*needles: str) -> str | None:
        return _find_col(headers, *needles)

    metric_cols = {
        "iops_first": col("iops", "first"),
        "iops_last": col("iops", "last"),
        "mibps_first": col("mib/s", "first"),
        "mibps_last": col("mib/s", "last"),
        "entries_per_s_first": col("entries/s", "first"),
        "entries_per_s_last": col("entries/s", "last"),
        "mib_total_first": col("mib", "first"),
        "mib_total_last": col("mib", "last"),
        "entries_first": col("entries", "first"),
        "entries_last": col("entries", "last"),
        "time_ms_first": col("time ms", "first"),
        "time_ms_last": col("time ms", "last"),
        "cpu_pct_first": col("cpu", "first"),
        "cpu_pct_last": col("cpu", "last"),
        "io_lat_us_min": col("io lat us", "min"),
        "io_lat_us_avg": col("io lat us", "avg"),
        "io_lat_us_max": col("io lat us", "max"),
        "ent_lat_us_min": col("ent lat us", "min"),
        "ent_lat_us_avg": col("ent lat us", "avg"),
        "ent_lat_us_max": col("ent lat us", "max"),
    }

    out: list[PhaseRow] = []
    for row in rows:
        op = (row.get(op_col, "") if op_col else "").strip() or "?"
        metrics: dict[str, float | int | None] = {}
        for canonical, src in metric_cols.items():
            metrics[canonical] = _coerce_number(row.get(src, "")) if src else None
        out.append(PhaseRow(operation=op, metrics=metrics, raw=dict(row)))
    return out


def parse_jsonfile(path: Path) -> dict[str, Any] | None:
    """Parse elbencho --jsonfile output. Returns None if the file is missing."""
    if not path.is_file():
        return None
    try:
        with path.open("r", encoding="utf-8") as fh:
            return json.load(fh)
    except (json.JSONDecodeError, OSError):
        return None


def extract_percentiles(jsondata: dict[str, Any] | None) -> dict[str, dict[str, int | float]]:
    """Best-effort extraction of latency percentiles from the JSON dump.

    elbencho's JSON output structure varies between versions. We do a recursive
    walk for any dict whose keys look like percentile labels (p50, p99, p99.9...)
    and return them grouped by their parent operation label when discoverable.
    """
    result: dict[str, dict[str, int | float]] = {}
    if not jsondata:
        return result

    pct_pattern = re.compile(r"^p\d{1,3}(?:\.\d+)?$|^99\.9+$|^percentile_?\d", re.IGNORECASE)

    def walk(node: Any, parent_label: str = "default") -> None:
        if isinstance(node, dict):
            pct_here: dict[str, int | float] = {}
            for k, v in node.items():
                if isinstance(v, (int, float)) and pct_pattern.match(str(k)):
                    pct_here[str(k).lower()] = v
            if pct_here:
                result.setdefault(parent_label, {}).update(pct_here)
            for k, v in node.items():
                label = str(k) if isinstance(v, (dict, list)) else parent_label
                walk(v, label)
        elif isinstance(node, list):
            for item in node:
                walk(item, parent_label)

    walk(jsondata)
    return result
