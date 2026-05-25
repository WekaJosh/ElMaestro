"""elbencho-harness TUI (Textual).

Multi-screen interactive UI:
  - HomeScreen           main menu (Run / Browse / Compare / Quit)
  - PickConfigScreen     YAML file picker
  - RunScreen            shows expanded specs, launches the run, live progress
  - BrowseResultsScreen  list recent runs, open report.html in browser
  - CompareScreen        multi-select runs, render compare HTML

Public API:
  - BenchApp:        Textual App class
  - run_home():      open at the home menu (default for `bench` no-args)
  - run_tui(cfg):    open straight to a config's RunScreen
"""

from .app import BenchApp, run_home, run_tui

__all__ = ["BenchApp", "run_home", "run_tui"]
