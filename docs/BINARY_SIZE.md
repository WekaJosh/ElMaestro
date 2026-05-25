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

## Further Python slimming

| Change | Estimated savings | Effort | What we'd lose |
|---|---|---|---|
| Drop plotly-python, hand-write Plotly.js JSON emitter | 3–5 MB | medium (200 LOC) | Plotly's figure factory convenience |
| Drop jinja2, use Python f-strings | 1 MB | small | Inheritance / loop syntax niceties |
| Drop typer + click, use stdlib argparse | 1–2 MB | small | Auto-generated help formatting |
| Drop rich, use plain print + ANSI | 2–3 MB | medium | Tables, syntax highlighting |
| Drop pydantic v2, use dataclasses + validation | 4 MB | large | Schema validation ergonomics |
| Drop structlog, use stdlib logging | <1 MB | small | Structured logging niceties |
| Replace textual with raw curses | 1–3 MB | very large | The screen-based architecture |

**Realistic aggressive-Python target: ~18–22 MB.** All combined, several weeks of work, real loss of dev ergonomics.

## Language survey (full)

Comparison across every language I considered for a from-scratch rewrite. Estimates are for OUR specific use case (TUI + subprocess driver + YAML/JSON parsing + HTML report emission), with the binary stripped and reasonable optimization flags. Reference points come from comparable published tools.

| Language | Realistic size | TUI maturity | Cross-compile | Ecosystem fit | Tier |
|---|---|---|---|---|---|
| **Rust** | 4–8 MB | excellent (ratatui) | good | excellent | **S** |
| **Go** | 8–15 MB | excellent (bubbletea) | excellent | excellent | **S** |
| **Zig** | 2–5 MB | weak (no major lib) | excellent | growing | A |
| **C#** (Native AOT) | 10–15 MB | good (Terminal.Gui) | good | excellent | A |
| **Nim** | 1–3 MB | weak (illwill) | good | small | A |
| **C++** | 3–8 MB | ok (FTXUI, ncurses) | manual | very good | A |
| **Swift** | 8–15 MB | weak (no major lib) | weak on Linux | macOS-first | B |
| **D** | 5–10 MB | weak (terminal.d) | ok | small | B |
| **OCaml** | 3–6 MB | weak | weak | small | B |
| **Crystal** | 2–5 MB | weak | macOS issues | small | B |
| **C** | 1–3 MB | ncurses | manual | manual | B |
| **Kotlin/Native** | 5–12 MB | weak | ok | JVM-leaning | C |
| **F#** (Native AOT) | 10–15 MB | shared with C# | good | small for native | C |
| **Java + GraalVM** | 20–40 MB | none for terminal | tricky | huge but VM-biased | C |
| **Scala** | 20–40 MB (Native Image) | none | tricky | JVM-leaning | C |
| **Common Lisp** (SBCL) | 30–50 MB | weak | weak | niche | C |
| **Haskell** | 15–30 MB | brick (decent) | weak | small | C |
| **Erlang/Elixir** | 50+ MB (BEAM) | weak | bundle BEAM | not suited | D |
| **Node.js** (pkg) | 40–80 MB | ink (React-like) | excellent | huge | D |
| **Deno/Bun compile** | 60–100 MB | ink-equivalents | excellent | huge | D |
| **Python** (current) | 33 MB | textual | excellent | huge | (status quo) |
| **Python** (Nuitka) | 15–25 MB | textual | weak cross-platform | same as above | side option |
| **Ruby** (mruby-cli) | 15–30 MB | TTY toolkit | weak | medium | D |
| **Perl** (PAR) | 25–40 MB | Curses::UI | weak | mature but niche | D |
| **V** | 1–3 MB | built-in TUI | claimed | unstable | D |
| **Odin** | 1–3 MB | none | manual | gamedev-focused | D |

**Tier S:** strongly recommended. Mature ecosystem, good size, viable in 2–3 weeks.
**Tier A:** reasonable choices with real trade-offs (smaller binary OR larger ecosystem, pick one).
**Tier B:** would work but I'd push back. Either the TUI story is missing or the language community is too small to count on.
**Tier C:** possible but the cost/payoff is bad for this specific tool.
**Tier D:** actively recommend against (huge binaries, wrong fit, or both).

---

## Details on tier-S candidates

### Rust

The default recommendation. Best size/ergonomics/safety combination for a TUI driver.

| What we use | Rust equivalent | Size compiled |
|---|---|---|
| pydantic + pyyaml | `serde` + `serde_yaml` + `serde_json` | ~400 KB |
| csv parsing | `csv` crate | ~150 KB |
| subprocess + asyncio | std::process + tokio | stdlib + ~500 KB |
| jinja2 | `askama` (compile-time) or `tera` | 0 / ~300 KB |
| plotly-python | hand-roll Plotly.js JSON | ~50 KB of our code |
| typer + rich | `clap` + `console` | ~600 KB |
| textual | `ratatui` + `crossterm` | ~600 KB |
| ulid-py | `ulid` crate | ~30 KB |
| structlog | `tracing` | ~200 KB |

**Reference points (stripped releases):** ripgrep 5.2 MB, bat 6 MB, bottom 3.5 MB, starship 12 MB, helix 14 MB.

**Target: 4–8 MB.** With LTO, strip, and `panic=abort`. UPX could take it under 3 MB but breaks macOS code signing.

**What you also get:** startup ~10–50 ms (vs Python's 400–800 ms cold-start while plotly imports), clippy + rustc catching whole bug categories, smaller attack surface.

**Code volume estimate:** ~2300 LOC of Rust. Less than the current Python (~3000 LOC). The cost isn't writing it; it's verifying behavior parity (chart output byte-equivalent, manifest schema unchanged, real-WEKA runs produce the same numbers).

**Effort: 2–3 weeks.**

### Go

Easier cross-compile, faster initial productivity, larger binary. Charm's TUI ecosystem (bubbletea + lipgloss + bubbles) is arguably the nicest TUI toolkit on any platform.

| What we use | Go equivalent |
|---|---|
| pydantic + pyyaml | `gopkg.in/yaml.v3` + manual struct tags |
| jinja2 | stdlib `text/template` |
| typer | `cobra` or stdlib `flag` |
| textual | `bubbletea` + `lipgloss` + `bubbles` |
| asyncio | goroutines / channels (much nicer for our model) |
| plotly-python | hand-roll JSON (same as Rust) |

**Reference points:** glow 12 MB, gh CLI 25 MB (huge surface), soft-serve 25 MB, bubbletea hello-world 8 MB.

**Target: 8–15 MB.** Go's runtime + GC + static linking floor is ~2–3 MB; everything we'd ship on top puts us in the 8–15 range.

**What you also get:** trivial cross-compile (`GOOS=linux go build` and done, no toolchain wrangling). Faster initial development pace than Rust. Stable language semantics.

**Effort: 2–3 weeks.** Probably the *fastest* of any option once started.

---

## Details on tier-A candidates (smaller-but-rougher)

### Zig

Smallest realistic native binary. Language is excellent. Ecosystem is the weak point.

**Target: 2–5 MB.**

What works:
- `std.process` covers subprocess lifecycle cleanly
- `std.json` is in-tree; YAML via [zig-yaml](https://github.com/kubkon/zig-yaml) is workable
- Tiny binaries because there's no runtime to bundle (you opt in to libc only when needed)

What doesn't:
- No mature TUI library. You'd build on top of termios + ANSI escape codes (a few hundred lines of glue) or wrap an existing C TUI lib like notcurses/termbox.
- Language is still pre-1.0; expect breaking changes between releases. Code from 6 months ago often won't compile.
- Smaller community means fewer Stack Overflow answers, smaller crate-equivalent set, less prior art for projects like this.

**Effort: 3–5 weeks** (mostly because of the TUI work and ecosystem gaps).

**Verdict:** worth considering only if 2–3 MB matters specifically. Most of the time, Rust's 4–8 MB is fine and the Rust ecosystem is two years ahead.

### C# Native AOT

The .NET story has changed. Native AOT (Ahead-Of-Time) shipped properly in .NET 8 and produces real single-file binaries with no .NET runtime install needed.

**Target: 10–15 MB.**

What works:
- Mature ecosystem: `System.Text.Json`, `YamlDotNet`, `Spectre.Console` for tables / progress, `Terminal.Gui` for full TUI.
- Excellent debugger and tooling (Rider, VS Code C# Dev Kit).
- Familiar OO model if you have Java/C# background.

What doesn't:
- Cross-compilation to Linux from macOS is doable but rougher than Rust/Go (`dotnet publish -r linux-x64 --self-contained -p:PublishAot=true`).
- AOT compilation isn't perfect; some reflection-heavy libraries don't work (luckily nothing we'd use).
- Binary sizes haven't quite caught up to Rust/Go yet because the .NET base library is large.

**Effort: 2–3 weeks** if you know C#; longer if not. .NET's TUI scene is smaller than Go's but bigger than Rust's.

**Verdict:** great choice if .NET is already a tooling preference. Otherwise Rust or Go gives you smaller binaries for the same effort.

### Nim

Nim compiles to C, then through your system C compiler. Tiny binaries, Python-like syntax.

**Target: 1–3 MB.**

What works:
- Smallest binaries of any "modern" language (the C compiler eliminates dead code aggressively).
- Python-ish syntax + significant whitespace + macros + native speed.
- `std/json`, `std/yaml` (via NimYAML), `std/osproc`.

What doesn't:
- Community is small. Most language ecosystems are 100x bigger.
- TUI options (`illwill`, `nimwave`) are workable but pre-1.0.
- Talent pool is narrow if you ever want to hand this off.

**Effort: 3–5 weeks** mostly for ecosystem gaps and TUI work.

**Verdict:** smallest binary I'd actually trust to ship. Real cost is "no one else will be able to maintain this."

### C++ (modern, C++20/23)

Smallest binaries among mainstream languages. Cost is engineering rigor.

**Target: 3–8 MB** with modern stdlib + curated deps.

What works:
- FTXUI is a genuinely good TUI library (functional reactive, modern C++).
- vcpkg / Conan for dependency management; nlohmann/json + yaml-cpp for parsers.
- Total control over allocations, layout, build flags.

What doesn't:
- Cross-compilation is manual (CMake + toolchain files for each target).
- Build times. Memory safety burden. Every bug is debugged with a debugger.
- Bigger code volume than Rust for the same functionality.

**Effort: 3–5 weeks** assuming the writer is comfortable with modern C++.

**Verdict:** only worth it if you genuinely want < 5 MB AND have C++ expertise on tap.

---

## Details on tier-B candidates (would work, but I'd push back)

**Swift.** Apple's language. On macOS the runtime is bundled in the OS (binaries are tiny). On Linux you need Swift's runtime, which is heavy and means dynamic library shipping. The Linux ecosystem is improving but still feels second-class. Real TUI lib is missing.

**D.** Good language. The community has shrunk over the years; library availability for serdes and TUI is thin. Two competing standard libraries (Phobos / Tango legacy) confuses newcomers.

**OCaml.** Excellent type system. TUI options (`lwt` async + `notty`) work but require ML-style functional thinking that fights against the imperative subprocess-driving model we'd want.

**Crystal.** Ruby-like syntax, compiles to native, small binaries. Cross-compilation from macOS is broken often enough that I wouldn't bet on it. TUI is weak.

**C.** Smallest possible binaries (1–3 MB) but you're writing in 1972 ergonomics. ncurses for TUI works fine. Total memory-safety burden falls on you for a tool whose value is benchmarking, not crypto. Skip unless you're optimizing for sub-2 MB binaries specifically.

---

## Tier-C / D: ruled out and why

- **Java / Scala / Kotlin.** All three can produce native binaries (GraalVM Native Image, Kotlin/Native) but the resulting binaries are 20–40 MB AND the toolchain setup is painful. The JVM legacy assumptions leak through.
- **Common Lisp (SBCL `save-image`).** Produces standalone binaries by dumping the running image. Excellent if you live in CL already. Niche for our purposes.
- **Haskell (GHC).** The `brick` TUI library is the best of any FP language, but binaries are 15–30 MB because GHC's runtime is heavy. Cross-compilation from macOS to Linux is well-known to be painful.
- **Erlang/Elixir.** Needs the BEAM runtime. ~50+ MB. Concurrency model is overkill for a subprocess driver.
- **Node.js / Deno / Bun compile.** Modern "compile to single binary" tools all embed V8 or JavaScriptCore. 50–100 MB binaries. Bigger than what we already have.
- **Ruby (`mruby-cli`).** Embedded Ruby works, but the ecosystem is smaller than Python's for our needs.
- **Perl (PAR::Packer).** Works, gives 25–40 MB binaries, but you're now in Perl.
- **V.** Language is still pre-1.0 and has had repeated public issues with stability claims. Cool idea, not for shipping production code today.
- **Odin.** Game-dev focused; no TUI ecosystem, small community.

---

## Recommendation, updated

If we ever want to ship this beyond internal use:

1. **Rust:** best overall. 4–8 MB binary, mature TUI (ratatui), excellent serde ecosystem for our YAML/JSON/CSV needs, ~2–3 week rewrite, future maintainers will thank us.

2. **Go:** best dev velocity. 8–15 MB binary (still 2x smaller than Python), the nicest TUI ecosystem (bubbletea/lipgloss), trivial cross-compile, possibly the fastest *initial* rewrite.

3. **Zig:** if 2–3 MB binary matters more than ecosystem maturity. Worth a serious look if you want the smallest possible thing AND are willing to write more TUI plumbing.

For internal-only use, **stay on Python at 33 MB** is also fine. The current binary works; the development pace is fast; the code is small enough to maintain.

### What I'd avoid no matter what

- Hybrid PyO3 / Cython for hot paths. Doesn't help binary size.
- Nuitka. Modest savings (15–25 MB), worse cross-platform story, not worth the toolchain swap.
- Anything requiring a VM at runtime (JVM, BEAM, .NET pre-Native-AOT).
- JavaScript / TypeScript compile-to-binary. Bigger than what we have today.
- Writing it in C without a strong reason. You'd save 1–2 MB over Rust at the cost of weeks more work and a permanent memory-safety burden.

---

## If we pick Rust, where to start

The pieces with the cleanest port (and the most test coverage to verify against):

1. **Schema models** (`config/models.py` → `src/config.rs`): serde derives, validators inline. ~300 LOC. Run the existing YAML fixtures through it, assert they parse to the same shape.
2. **Sweep expansion** (`config/sweep.py` → `src/sweep.rs`): pure Python today (no I/O, no concurrency). Trivial port; the existing tests give us byte-equivalent oracles.
3. **CSV / JSON result parsers** (`engine/elbencho.py` + `backends/fio.py` parse paths): pull the real WEKA-captured CSV/JSON fixtures over and assert identical PhaseResult fields.
4. **Backends trait** (`backends/base.py` → `trait Backend`): straightforward translation; the protocol is small.

The harder pieces (HTML report emission, TUI screens, subprocess + SSH layer) come after we have a working pipeline that parses configs and produces result.json files at byte-equivalent shape.
