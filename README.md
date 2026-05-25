# elbencho-harness

Interactive IO benchmarking harness. Drives [elbencho](https://github.com/breuner/elbencho) or [fio](https://github.com/axboe/fio); pick the engine in YAML, the rest of the harness (sweeps, compare, TUI, SSH fan-out) stays engine-agnostic. Targets local mount points, S3 endpoints (elbencho only), or multi-client fleets via SSH. Renders self-contained HTML reports.

## Status

**v0.7.** Engines: elbencho (POSIX + S3) and fio (POSIX). Multi-client SSH fan-out for both. Sweep expansion (cartesian / ladder), resume, compare reports, Textual TUI. See [docs/PLAN.md](./docs/PLAN.md) for design notes.

## Install

You only need one of these. The binary requires nothing but the engine you want to drive (elbencho or fio).

### Option A: prebuilt binary (recommended)

Single-file executable, ~45–60 MB. Bundles the Python interpreter and every Python dep. Drop it on PATH and run.

```bash
# 1. Download the right binary for your machine (see dist/ in this repo,
#    or grab from the GitHub Releases page once a release is cut):
#      elmaestro-macos-arm64       Apple Silicon
#      elmaestro-linux-x86_64      Linux x86_64
curl -L -o /usr/local/bin/elmaestro https://example.com/elmaestro-linux-x86_64
chmod +x /usr/local/bin/elmaestro

# 2. Install the engine on every machine that will actually run IO:
#    elbencho: https://github.com/breuner/elbencho/releases (Linux),
#              build from source on macOS
#    fio:      apt install fio  /  dnf install fio  /  brew install fio

# 3. Launch (no args = open the TUI):
elmaestro
```

### Option B: from source (developers / contributors)

Requires Python 3.11+.

```bash
git clone https://github.com/WekaJosh/ElMaestro.git
cd ElMaestro
python3.11 -m venv .venv
.venv/bin/pip install -e ".[dev,ssh,tui]"
.venv/bin/bench       # same TUI, same subcommands
```

`dev` installs pytest + ruff, `ssh` adds asyncssh (multi-client fan-out), `tui` adds Textual. All three are optional; the harness runs with just the base deps for single-host local POSIX.

### Building your own binary

```bash
.venv/bin/pip install pyinstaller
scripts/build-binary.sh
# Output: dist/elmaestro-<os>-<arch>
```

## Commands

`elmaestro` with no arguments opens the TUI home menu (run / browse / compare / quit). Everything else stays available as a subcommand for scripted use:

```bash
elmaestro                                       # open TUI (default)
elmaestro version                               # print version
elmaestro validate examples/single_test.yaml    # parse + show summary
elmaestro expand   examples/sweep_block_sizes.yaml  # dry-run a sweep (no execution)
elmaestro run      examples/single_test.yaml    # execute, write results, render report
elmaestro run      <cfg> --resume results/<run-dir>   # skip completed specs on retry
elmaestro report   results/<run-dir>/           # re-render report.html from result.json
elmaestro compare  results/A/ results/B/        # overlay multiple runs in one HTML
elmaestro tui      examples/sweep_block_sizes.yaml    # jump straight to a config's run screen
```

From source: replace `elmaestro` with `.venv/bin/bench` (both entry points are identical).

## What's in a run

Each `bench run` produces a directory like:

```
results/2026-05-22T14-03-11_sweep_bs-scan/
├── manifest.json
├── 0001_local-tmp_seq-read-base_bs-64KiB/
│   ├── result.json              # canonical result schema (v1.0)
│   ├── report.html              # per-spec Plotly report
│   └── raw/
│       ├── run.csv              # elbencho --csvfile output
│       ├── run.json             # elbencho --jsonfile output
│       ├── stdout.log
│       └── stderr.log           # if elbencho wrote to stderr
├── 0002_local-tmp_seq-read-base_bs-256KiB/
│   └── ...
└── report.html                  # top-level pointer report
```

## Configs

A config is YAML with these top-level keys: `engine` (optional, default `elbencho`; alternative `fio`), `clients`, `targets`, `workloads`, plus either `runs:` (explicit list) or `sweeps:` (one config, many specs). See [examples/](./examples) for working configs covering:

- `single_test.yaml`: one POSIX test against `/tmp` (elbencho)
- `fio_single.yaml`: same shape, fio engine
- `multi_client.yaml`: three SSH workers, single workload
- `s3_minio.yaml`: S3 target with multipart + object prefix (elbencho only)
- `sweep_block_sizes.yaml`: five-point cartesian sweep across block sizes

The Workload model is shared across engines. Each backend translates the common fields (`block_size`, `threads_per_client`, `io_depth`, `direct_io`, `file_size`, etc.) to its native flags. Use `extra_flags:` on a workload to inject engine-specific tuning the schema doesn't model yet (lines are appended verbatim to fio job files, or to elbencho's command line).

**Engine-specific notes:**
- **elbencho**: supports both POSIX and S3 targets. Requires the binary to be built with `S3_SUPPORT=1` for S3 runs.
- **fio**: POSIX targets only in v0.7. The harness writes a tiny job file to `raw/job.fio` and references it; for multi-client runs, the master fans out via `--client=host:port`.

## Roadmap

See [docs/PLAN.md](./docs/PLAN.md). v0.6 covers everything originally planned; longer-term ideas (full TUI editor, vendored elbencho, MinIO CI fixture) are listed there.
