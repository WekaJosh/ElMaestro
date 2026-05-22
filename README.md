# elbencho-harness

Interactive IO benchmarking harness built on [elbencho](https://github.com/breuner/elbencho). Drives single tests or multi-axis sweeps against local mount points, S3 endpoints, or multi-client fleets via SSH. Renders self-contained HTML reports and supports overlay comparisons across runs.

## Status

**v0.6.** Local POSIX, S3 targets, multi-client SSH fan-out, sweep expansion (cartesian / ladder), resume, compare reports, Textual TUI. See [docs/PLAN.md](./docs/PLAN.md) for design notes.

## Install

Requirements: Python 3.11+ and (for real runs) the [elbencho](https://github.com/breuner/elbencho) binary on the coordinator and every worker.

```bash
# 1. Clone and enter the repo
git clone https://github.com/WekaJosh/ElMaestro.git
cd ElMaestro

# 2. Create a venv and install. pip works; uv works; pick one.
python3.11 -m venv .venv   # or `python3 -m venv .venv` if your default is 3.11+
.venv/bin/pip install -e ".[dev,ssh,tui]"

# 3. Install elbencho on every machine that will run workloads.
# Linux: grab a release binary from https://github.com/breuner/elbencho/releases
# macOS: build from source (see the elbencho README)
```

The `dev` extra installs pytest + ruff, `ssh` adds asyncssh (multi-client fan-out), and `tui` adds Textual. All three are optional; the harness runs with just the base deps for single-host local POSIX work.

## Commands

```bash
.venv/bin/bench version                                  # print version
.venv/bin/bench validate examples/single_test.yaml       # parse + show summary
.venv/bin/bench expand   examples/sweep_block_sizes.yaml # dry-run a sweep (no execution)
.venv/bin/bench run      examples/single_test.yaml       # execute, write results, render report
.venv/bin/bench run      <cfg> --resume results/<run-dir>  # skip completed specs on retry
.venv/bin/bench report   results/<run-dir>/              # re-render report.html from result.json
.venv/bin/bench compare  results/A/ results/B/           # overlay multiple runs in one HTML
.venv/bin/bench tui      examples/sweep_block_sizes.yaml # Textual UI: live spec progress
```

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

A config is YAML with four sections: `clients`, `targets`, `workloads`, plus either `runs:` (explicit list) or `sweeps:` (one config, many specs). See [examples/](./examples) for working configs covering:

- `single_test.yaml`: one POSIX test against `/tmp`
- `multi_client.yaml`: three SSH workers, single workload
- `s3_minio.yaml`: S3 target with multipart + object prefix
- `sweep_block_sizes.yaml`: five-point cartesian sweep across block sizes

## Roadmap

See [docs/PLAN.md](./docs/PLAN.md). v0.6 covers everything originally planned; longer-term ideas (full TUI editor, vendored elbencho, MinIO CI fixture) are listed there.
