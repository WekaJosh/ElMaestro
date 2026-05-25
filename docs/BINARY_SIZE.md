# Binary size: what we cut, what's left, what a rewrite would buy

This is a working doc on the size question. It exists because "the binary is too big" came up in v0.8 and the answer turned out to be "yes, but the obvious culprits aren't always the obvious ones."

## Trajectory so far

| Version | macOS arm64 | Linux x86_64 | What changed |
|---|---|---|---|
| v0.8.0 | 58 MB | 43 MB | First PyInstaller release |
| v0.8.1 | 38 MB | 38 MB | Dropped pandas/numpy/Jupyter; optimize=2 + strip |
| **v0.9.0** | **33.5 MB** | **33.1 MB** | Dropped asyncssh; use system ssh(1) |

42% smaller than the first release.

## What's actually in the 33 MB

Measured via `pyi-archive_viewer -l`, top contributors after v0.9.0:

| Component | Size in bundle | Why it's there | Can we cut it? |
|---|---|---|---|
| PYZ (compressed Python bytecode) | 25 MB | plotly, pydantic, jinja2, textual, rich, typer, ulid, structlog, pyyaml, humanfriendly | Yes, per-dep work |
| Python framework binary | 5.4 MB | The interpreter itself | No (floor for any Python distribution) |
| python3 (interpreter + stdlib bits) | 6.4 MB | More of the same | No |
| libcrypto.3.dylib | 4.8 MB | Required by stdlib `ssl` + `hashlib` | No (would lose TLS support) |
| plotly (raw .py data) | 4.7 MB | Our chart library | Yes, ~5 MB if we drop it |
| pydantic_core (Rust) | 4.2 MB | Schema validation | Yes, ~4 MB if we drop pydantic |
| libssl, libsqlite3, libzstd, libmpdec, liblzma | ~3 MB combined | Python stdlib | No |
| base_library (Python stdlib subset) | 1.3 MB | Required | No |

The non-negotiable Python floor is roughly **20 MB**: interpreter + framework + base stdlib + the libs it dynamically links against. Anything else is a choice we made.

## Further Python slimming (what each cut would buy)

| Change | Estimated savings | Effort | What we'd lose |
|---|---|---|---|
| Drop plotly-python, hand-write Plotly.js JSON emitter | 3–5 MB | medium (200 LOC) | Plotly's figure factory convenience; we'd build the JSON structure directly |
| Drop jinja2, use Python f-strings / `string.Template` | 1 MB | small | Inheritance / loop syntax niceties; we have ~3 templates |
| Drop typer + click, use stdlib argparse | 1–2 MB | small | Auto-generated help formatting (already pretty plain) |
| Drop rich, use plain print + ANSI codes | 2–3 MB | medium | Tables, syntax highlighting; we use a small slice of rich |
| Drop pydantic v2, use dataclasses + manual validation | 4 MB (pydantic_core .so) | large | Schema validation ergonomics; would need to write per-field checks |
| Drop structlog, use stdlib logging | <1 MB | small | Structured logging niceties (not heavily used yet) |
| Replace textual with prompt_toolkit (smaller) or write minimal TUI on raw curses | 1–3 MB | very large | The screen-based architecture; we'd rebuild it |

**Realistic aggressive-Python target: ~18–22 MB.** All of the above combined, several weeks of work, real loss of dev ergonomics. Each individual cut is fine; the aggregate is a Python rewrite of half our deps.

## What a Rust rewrite would look like

The Rust ecosystem has direct or near-direct replacements for every dep we use:

| What we use | Rust equivalent | Size compiled |
|---|---|---|
| pydantic + pyyaml | `serde` + `serde_yaml` + `serde_json` | ~400 KB total |
| csv parsing | `csv` crate | ~150 KB |
| subprocess.run + asyncio | std::process + tokio | bundled in stdlib + ~500 KB |
| jinja2 | `askama` (compile-time templates) or `tera` | 0 KB (askama) / ~300 KB (tera) |
| plotly-python | hand-roll Plotly.js JSON emission | ~50 KB of our own code |
| typer + rich | `clap` + `console`/`ratatui-style printing` | ~600 KB |
| textual | `ratatui` + `crossterm` | ~600 KB |
| ulid-py | `ulid` crate | ~30 KB |
| structlog | `tracing` crate | ~200 KB |

**Reference points for comparable Rust TUIs:**
- ripgrep: 5.2 MB
- bat (syntax-highlighting cat): 6 MB
- bottom (htop with TUI): 3.5 MB
- starship (prompt): 12 MB
- helix (full text editor): 14 MB

**Realistic Rust target: 4–8 MB.** Stripped, with LTO. Could probably hit 3 MB with UPX (we'd lose macOS code signing).

What you'd also get for free in Rust:
- Startup ~10–50 ms instead of Python's ~400–800 ms (plotly imports dominate on cold start)
- No `optimize=2 / strip` tweakery; Rust just emits a small binary by default
- Static analysis: clippy + rustc together catch the kinds of bugs we currently rely on tests for
- Smaller attack surface (no embedded Python interpreter)

The actual code volume isn't huge:
- Config schemas in serde: ~300 LOC
- Sweep expansion: ~200 LOC
- elbencho + fio backends: ~600 LOC combined
- HTML report emission: ~400 LOC (the biggest unknown; building Plotly.js JSON by hand)
- TUI screens with ratatui: ~600 LOC
- subprocess/ssh layer: ~200 LOC

Estimate: ~2300 LOC of Rust. Compared to ~3000 LOC of Python today (excluding tests). The work isn't writing more code; it's making behavior parity with the Python version verifiable.

**Effort estimate: 2–3 weeks of focused work.** Includes rewriting tests in `cargo test`, getting the chart output byte-equivalent to what plotly-python emits, and verifying against the real WEKA box.

## What a Go rewrite would look like

Direct parallels exist for everything:

| What we use | Go equivalent |
|---|---|
| pydantic + pyyaml | `gopkg.in/yaml.v3` + manual struct validation |
| jinja2 | stdlib `text/template` |
| typer | `cobra` or stdlib `flag` |
| textual | `bubbletea` (Charm) + `lipgloss` |
| asyncio | goroutines / channels |
| plotly-python | hand-roll JSON emission (same as Rust path) |

**Reference points for comparable Go TUIs:**
- glow (markdown TUI): 12 MB
- gh CLI: 25 MB (huge feature set)
- soft-serve (git TUI): 25 MB
- A bubbletea hello-world: ~8 MB

**Realistic Go target: 8–15 MB.** Bigger than Rust because Go's runtime + GC are ~2 MB plus everything is statically linked.

What you'd get: easier cross-compilation than Rust (`GOOS=linux go build` and you're done), arguably faster development pace, slightly larger binaries.

## Recommendation

Three honest options:

1. **Stay on Python, ship 33 MB.** This is fine for an internal SE tool. The binary works, the development pace is fast, the codebase is small enough to maintain. Don't fix what isn't broken.

2. **Aggressive Python slimming to ~20 MB.** Each step is straightforward but the aggregate is multiple weeks. You lose dev ergonomics (no pydantic, no rich, manual templates) and gain about 13 MB. Probably not worth it.

3. **Rust rewrite to ~5 MB.** Real one-time cost (2-3 weeks). Permanent benefits: 7x smaller, 10-30x faster startup, easier distribution, smaller attack surface. The Python version becomes the prototype.

If we ever want to share this with customers or ship it as part of WEKA's tooling story, option 3 is the right answer. For internal SE work, option 1 is fine.

## What I would NOT recommend

- **Hybrid: PyO3 hot paths.** Doesn't help binary size; Python interpreter + stdlib is the floor.
- **Nuitka instead of PyInstaller.** Smaller binaries in some cases, but the macOS arm64 + cross-compilation story is rougher; not a meaningful win for the effort.
- **Bundle a stripped-down libcrypto.** Possible but breaks Python's `ssl` and `hashlib`; we'd lose more than we save.
- **Drop the engine binary deps and reimplement IO benchmarking ourselves.** That's a different project (and a much harder one).
