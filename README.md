# ElMaestro

Interactive IO benchmarking harness. Drives [elbencho](https://github.com/breuner/elbencho) or [fio](https://github.com/axboe/fio); pick the engine in YAML, the rest of the harness (sweeps, compare, TUI, SSH fan-out) stays engine-agnostic. Targets local mount points, S3 endpoints (elbencho only), or multi-client fleets via SSH. Renders self-contained HTML reports.

## Status

Local POSIX, S3 targets (elbencho), multi-client SSH fan-out (keys, password via sshpass, jump hosts), bash-style brace expansion for host lists (`10.10.10.{1..100}`), sweep expansion (cartesian / ladder), explicit dataset-layout phase before read tests, live progress in the TUI, in-TUI report and compare viewers, save/load templates, pre-flight validation (`check`), client hardware capture in every result, HTML reports. Single static binary, 2-3 MB depending on platform.

## Install

Single-file binary, nothing else needed but the engine you want to drive. The `latest/download` URLs below always fetch the newest release ([release history](https://github.com/WekaJosh/ElMaestro/releases)).

```bash
# Pick your platform:
# Linux x86_64
curl -L -o /usr/local/bin/elmaestro \
  https://github.com/WekaJosh/ElMaestro/releases/latest/download/elmaestro-linux-x86_64
# Linux arm64 (Grace, Graviton, Ampere, ...)
curl -L -o /usr/local/bin/elmaestro \
  https://github.com/WekaJosh/ElMaestro/releases/latest/download/elmaestro-linux-arm64
# macOS (Apple Silicon)
curl -L -o /usr/local/bin/elmaestro \
  https://github.com/WekaJosh/ElMaestro/releases/latest/download/elmaestro-macos-arm64

chmod +x /usr/local/bin/elmaestro

# Install the engine on every machine that will run IO:
#   elbencho: https://github.com/breuner/elbencho/releases (Linux),
#             build from source on macOS
#   fio:      apt install fio  /  dnf install fio  /  brew install fio
# Optional: sshpass on the coordinator if any worker uses password auth.

# Launch (no args opens the TUI):
elmaestro
```

The Linux binaries are static (musl), so they run on any distro regardless of glibc version.

## Commands

`elmaestro` with no args opens the TUI home menu. Subcommands stay available for scripted use:

```bash
elmaestro                                       # open TUI
elmaestro version                               # print version
elmaestro validate examples/single_test.yaml    # parse + show summary
elmaestro expand   examples/sweep_block_sizes.yaml  # dry-run a sweep
elmaestro check    examples/multi_client.yaml   # pre-flight: ssh, engine binaries, S3, mounts
elmaestro run      examples/single_test.yaml    # execute (runs check first; --no-check to skip)
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
- **fio**: POSIX only. Multi-client via `fio --server` on each worker; the master drives all workers through a generated hosts file.

**Jump hosts:** when a client sets `ssh_jump` (or the TUI's Jump host field), the bastion becomes the coordinator: the engine master process runs there, engine version checks target it, and service-port probes run on the workers themselves. Your laptop needs neither the engine binary nor network reachability to the workers — only SSH to the bastion. The bastion needs the engine binary and must authenticate with keys/agent (worker passwords still work via sshpass). Accepts `host`, `user@host`, or `user@host:port`.

## What's in a run

```
results/2026-05-25T14-03-11_sweep_bs-scan/
├── manifest.json            # spec index + sweep axis values (feeds compare)
├── 0001_local-tmp_seq-read-base_bs-64KiB/
│   ├── result.json          # canonical result schema (incl. client hardware)
│   ├── report.html          # per-spec Plotly report
│   ├── raw_layout/          # dataset-staging pass (read tests only)
│   └── raw/
│       ├── run.csv          # engine native output (elbencho)
│       ├── run.json         # engine native output (fio: one dump per second)
│       ├── live.csv         # live stats (elbencho)
│       ├── hosts.list       # workers driven (fio multi-host)
│       ├── command.txt      # exact engine invocation
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

## Roadmap

See [docs/PLAN.md](./docs/PLAN.md).
