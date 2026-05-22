# elbencho-harness roadmap

Living design doc. Phases are roughly ordered by value to a WEKA SE running customer POVs,
not by what's easiest. Each phase ships a working `bench run` end to end. No half-states.

## Where we are

**v0.1 (shipped, commit a7016f5):** local POSIX, single host, single test per invocation,
self-contained HTML report. Pydantic models already define the shape of multi-client and
sweep configs but the loader and coordinator only honor `runs:` against `localhost`.

What works today:
- `bench validate` / `bench run` / `bench report` / `bench version`
- POSIX target, sequential or random, read/write mix, direct IO, drop-caches
- `result.json` (schema 1.0), `manifest.json`, single-run Plotly report

What doesn't:
- Sweeps (`SweepAxis`, `Sweep` defined but never expanded into `RunSpec` list)
- Multi-host (single `ClientHost` only; `ssh_user`, `service_port` etc. ignored)
- S3 (`S3Target` defined, never reaches elbencho)
- Compare report (no way to view N runs side by side)
- TUI (`textual` reserved in pyproject, not installed)

## Phase plan

Order is debatable. Recommendation: SSH fan-out first, because single-client benchmarks
of a parallel file system don't tell you anything useful. Sweeps without multi-client just
sweep one underpowered point. Compare report comes after both feed it real data.

### v0.2: SSH fan-out (multi-client)

**Goal:** one `bench run` orchestrates elbencho across N hosts and merges their results
into a single `result.json`.

**Scope:**
- Async SSH (asyncssh) to each `ClientHost` to start `elbencho --service` and reach it on
  `service_port` (default 1611).
- Coordinator launches one master + N service hosts per elbencho's normal multi-client model.
- Results aggregated from elbencho's `--hosts` CSV/JSON; per-client breakdown preserved in
  `raw/`, headline numbers in `result.json`.
- Health check phase: SSH reachable, elbencho version matches across hosts, mount/bucket
  exists on each, clock skew under a threshold.
- Failure modes: partial host failure (warn + continue with degraded count, marked in
  manifest) vs total failure (abort run cleanly).

**Design decisions to make:**
- One master per run, or rotate? (elbencho convention: one master, N services)
- SSH key strategy: explicit `ssh_key` per host, fall back to agent, or both?
- Service lifecycle: leave running between runs or stop/start per spec? (leave running is
  faster but leaks state; stop/start is cleaner)
- How to surface per-client variance in the report (boxplot? per-host bars?)

**Open questions:**
- Do we need a `bench services start|stop|status` admin command, or fold lifecycle into `run`?
- elbencho service mode auth: is the default OK or do we need a shared secret?

**Dependencies:** `asyncssh>=2.14` (already commented in pyproject under `[ssh]` extra).

---

### v0.3: Sweep expansion

**Goal:** a single config can expand to dozens of `RunSpec`s across one or more axes, all
written into one run directory with a single rolled-up report.

**Scope:**
- `Sweep` -> `list[RunSpec]` expansion in the loader (or a new `sweep.expand()`).
- Two orders: `cartesian` (full product) and `ladder` (vary one axis at a time, baseline
  for everything else). Document the trade-off.
- `max_runs` cap to prevent accidentally launching 4096-point sweeps.
- Manifest tracks the sweep's identity, axes, and per-spec status.
- Resume: if `manifest.json` exists and a spec_hash already has `completed`, skip on retry.

**Design decisions:**
- Where does the sweep axis become a column in the report? (need to flatten `RunSpec` into
  a tidy table the renderer can groupby on)
- Naming the spec directories: index-based (`0001_…`) is OK but doesn't surface the axis
  values. Consider `0001_bs=1MiB_t=4_…` (clipped) for grep-ability.
- Cross-product with multi-target: a sweep can name multiple `targets:`, does that
  cartesian out too?

**Open questions:**
- Do we want a `bench expand <config>` dry-run that prints all RunSpecs without executing?
  (Useful for sanity-checking before a long sweep.)

**Dependencies:** none new; depends on v0.2 only if you want client_count as a sweep axis.

---

### v0.4: Compare report

**Goal:** load N run directories and produce one HTML that overlays them. Same axes,
same chart types, with run labels as the series.

**Scope:**
- `bench report --compare run-A/ run-B/ run-C/` (or `bench compare …`).
- Aligns runs by workload identity (workload name + relevant axis values).
- Per-metric overlay: throughput, IOPS, latency percentiles.
- Diff table: percentage delta vs first run as baseline.
- Same self-contained HTML output (no server).

**Design decisions:**
- How strict is the workload-identity match? (probably: exact `spec_hash` matches collapse,
  otherwise show separate series with full label)
- What does "baseline" mean (first arg, or a `--baseline run-A/` flag)?

**Dependencies:** v0.3 makes this 10x more useful (sweep comparisons across hardware
configs), but the feature itself just needs multiple `result.json` directories.

---

### v0.5: S3 targets

**Goal:** the `S3Target` model actually works end to end against WEKA S3, AWS S3, and
MinIO.

**Scope:**
- Wire `S3Target` into the elbencho command builder (`--s3endpoints`, `--bucket`, region,
  path-style addressing).
- Credentials resolution from `credentials_ref` (`env:NAME` or `file:/path`).
- S3-specific knobs in `Workload`: multipart threshold, object key prefix layout.
- Validate against MinIO in CI fixture; document the WEKA S3 quirks (e.g. endpoint format,
  region handling).

**Open questions:**
- How does this interact with sweeps? (e.g. sweep `multipart_threshold` is meaningful for
  S3 only)
- Bucket cleanup behavior on `cleanup: true`: match the POSIX semantics, or skip and
  document?

---

### v0.6: TUI

**Goal:** interactive config builder + live progress for long sweeps.

**Scope:**
- Textual-based TUI: load a YAML, edit fields against the Pydantic schema, validate as
  you type, kick off `bench run` and watch progress.
- Live per-spec status table (queued, running, completed, failed), same data as
  `manifest.json`, just rendered live.
- Optional: a "compare runs" view that picks two run directories and shows the v0.4 diff
  inline.

**Dependencies:** `textual>=0.80`, `textual-plotext` (reserved in pyproject under `[tui]`
extra). Probably needs structlog -> rich/textual bridge for clean log capture.

---

## Non-goals (for the foreseeable future)

- Continuous benchmarking / scheduled runs / CI integration
- Storing results in a database (filesystem layout is the source of truth)
- Web UI / hosted dashboard
- Anything that requires running as root beyond what elbencho already needs (drop-caches)
- Driving non-elbencho tools (fio, iozone). If we want that later it's a sibling project,
  not a backend swap.
- Cross-cloud cost reporting

## Things to revisit before v0.3

- Hashing: `RunSpec.make_spec_hash` includes the full client list. That's correct for
  fingerprinting an exact run, but it means two runs with N=4 clients that happen to use
  different hostnames have different hashes even if the workload is identical. May want a
  second `workload_hash` for compare-report grouping.
- `results-smoke/` vs `results/` split exists only because the smoke fixture wrote to a
  different dir. Once sweep + multi-client land, real runs will produce nested structures
  that the smoke fixture should mirror.

## Open questions, not phase-bound

- elbencho's `--csvfile` and `--jsonfile` outputs don't fully overlap; we parse both. Long
  term, pick one and document why.
- macOS dev experience: `brew install elbencho` doesn't exist. Either ship a small build
  script, or accept that macOS dev uses the fake fixture and real runs happen on Linux.
- Should we vendor a pinned elbencho version (download a release binary on first run)?
  Trade-off: reproducibility vs surprise downloads.
