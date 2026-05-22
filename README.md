# elbencho-harness

Interactive IO benchmarking harness built on [elbencho](https://github.com/breuner/elbencho). Drives single tests or multi-axis sweeps against local mount points or S3 endpoints, and renders self-contained HTML reports.

## Status

**v0.1** — local POSIX, single-host, single test, basic HTML report. Multi-client SSH fan-out, sweeps, S3, TUI, and compare reports land in later phases.

## Install

```bash
# 1. Install uv (Python project manager)
brew install uv
# or: curl -LsSf https://astral.sh/uv/install.sh | sh

# 2. Sync deps
uv sync

# 3. Install elbencho (the underlying benchmark tool)
# macOS: build from source (see https://github.com/breuner/elbencho)
# Linux: grab a release binary from https://github.com/breuner/elbencho/releases
```

## Quick start

```bash
# Validate a config without running anything
uv run bench validate examples/single_test.yaml

# Run a single test (writes results to ./results/<run-id>/)
uv run bench run examples/single_test.yaml

# Re-render the HTML report from existing results
uv run bench report results/<run-id>/

# Show version
uv run bench version
```

## What's in a run

Each `bench run` produces a directory like:

```
results/2026-05-22T14-03-11_localmnt-1m-seq-read/
├── manifest.json
├── 0001_localmnt_1m-seq-read/
│   ├── result.json              # canonical result schema
│   └── raw/
│       ├── run.csv              # elbencho --csvfile output
│       ├── run.json             # elbencho --jsonfile output
│       └── stdout.log           # captured stdout
└── report.html                  # self-contained Plotly report
```

## Roadmap

See [the design plan](./docs/PLAN.md) for the full multi-phase roadmap (SSH fan-out, sweeps, S3, TUI, compare).
