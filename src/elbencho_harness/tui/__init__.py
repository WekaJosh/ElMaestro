"""elbencho-harness TUI (Textual).

Single-screen interactive runner: shows the expanded spec list from a config,
launches the run, and updates live as each spec completes.

Public API:
  - BenchApp:        Textual App class (use via `BenchApp(config=path).run()`)
  - run_tui(config): convenience entry the CLI uses
"""

from .app import BenchApp, run_tui

__all__ = ["BenchApp", "run_tui"]
