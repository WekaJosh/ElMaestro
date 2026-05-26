# ElMaestro

Interactive IO benchmarking harness. Drives [elbencho](https://github.com/breuner/elbencho) or [fio](https://github.com/axboe/fio); pick the engine in YAML, the rest of the harness (sweeps, compare, TUI, SSH fan-out) stays engine-agnostic. Targets local mount points, S3 endpoints (elbencho only), or multi-client fleets via SSH. Renders self-contained HTML reports.

## Status

**v1.0.0.** Rust rewrite of the original Python harness. Same feature set (engines, sweeps, resume, SSH fan-out, compare reports, TUI), 16x smaller binary, much faster startup. Python source preserved under `python-legacy/` for reference; see [docs/BINARY_SIZE.md](./docs/BINARY_SIZE.md) for the rewrite rationale.

| Build | macOS arm64 | Linux x86_64 |
|---|---|---|
| Python v0.9.0 (PyInstaller) | 33.5 MB | 33.1 MB |
| **Rust v1.0.0** | **2.1 MB** | **2.9 MB** |

## Install

Single-file binary, nothing else needed but the engine you want to drive.

```bash
# Download for your platform (linux x86_64 shown):
curl -L -o /usr/local/bin/elmaestro \
  https://github.com/WekaJosh/ElMaestro/releases/download/v1.0.0/elmaestro-linux-x86_64
chmod +x /usr/local/bin/elmaestro

# Install the engine on every machine that will run IO:
#   elbencho: https://github.com/breuner/elbencho/releases (Linux),
#             build from source on macOS
#   fio:      apt install fio  /  dnf install fio  /  brew install fio

# Launch (no args opens the TUI):
elmaestro
```

## Commands

`elmaestro` with no args opens the TUI home menu. Subcommands stay available for scripted use:

```bash
elmaestro                                       # open TUI
elmaestro version                               # print version
elmaestro validate examples/single_test.yaml    # parse + show summary
elmaestro expand   examples/sweep_block_sizes.yaml  # dry-run a sweep
elmaestro run      examples/single_test.yaml    # execute, write result.json + report.html
elmaestro report   results/<run-dir>/           # re-render report.html from result.json
elmaestro compare  results/A/ results/B/        # overlay multiple runs in one HTML
elmaestro tui      examples/sweep_block_sizes.yaml    # TUI for a specific config
```

## Configs

A config is YAML with these top-level keys: `engine` (optional, default `elbencho`; alternative `fio`), `clients`, `targets`, `workloads`, plus either `runs:` (explicit list) or `sweeps:` (one config, many specs). See [examples/](./examples) for working configs covering:

- `single_test.yaml`: one POSIX test against `/tmp` (elbencho)
- `fio_single.yaml`: same shape, fio engine
- `multi_client.yaml`: three SSH workers, single workload
- `s3_minio.yaml`: S3 target with multipart + object prefix (elbencho only)
- `sweep_block_sizes.yaml`: five-point cartesian sweep across block sizes

The Workload model is shared across engines. Each backend translates the common fields (`block_size`, `threads_per_client`, `io_depth`, `direct_io`, `file_size`, etc.) to its native flags. Use `extra_flags:` to inject engine-specific tuning.

**Placeholders:** YAML files support `${CONFIG_DIR}`, `${HOME}`, and `$ENV{NAME}` substitution before parsing.

**Engine-specific notes:**
- **elbencho**: POSIX + S3. Requires the binary built with `S3_SUPPORT=1` for S3.
- **fio**: POSIX only in v1.0. Multi-client via `fio --server` / `--client=host,port`.

## What's in a run

```
results/2026-05-25T14-03-11_sweep_bs-scan/
├── 0001_local-tmp_seq-read-base_bs-64KiB/
│   ├── result.json          # canonical result schema v1.0
│   ├── report.html          # per-spec Plotly report
│   └── raw/
│       ├── run.csv          # engine native output (elbencho)
│       ├── run.json         # engine native output
│       ├── stdout.log
│       └── stderr.log       # only if engine wrote to stderr
└── ...
```

## Building from source

Requirements: Rust 1.75+. Clone and `cargo build --release`.

```bash
git clone https://github.com/WekaJosh/ElMaestro.git
cd ElMaestro
cargo build --release
# Output: target/release/elmaestro
```

## Why the rewrite?

The original Python implementation (preserved under `python-legacy/`) shipped a 33 MB PyInstaller binary that bundled the Python interpreter, plotly, pydantic, textual, and asyncssh + cryptography + OpenSSL. The Rust rewrite drops it to 2-3 MB with zero feature loss. See [docs/BINARY_SIZE.md](./docs/BINARY_SIZE.md) for the full analysis including realistic targets for every other commonly-used language.

## Roadmap

See [docs/PLAN.md](./docs/PLAN.md).
