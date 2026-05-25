"""Textual app entry. Thin shell that pushes the right initial screen.

Two entry modes:
  - `run_home()`     opens at HomeScreen. The default when `bench` is invoked
                     with no arguments.
  - `run_tui(cfg)`   opens straight to RunScreen for the given config. Used by
                     `bench tui <config>` for scripted flows.

Both call BenchApp.run(), which blocks until the user quits.
"""

from __future__ import annotations

from pathlib import Path
from typing import ClassVar

from textual.app import App
from textual.binding import Binding

from .screens import (
    BrowseResultsScreen,
    CompareScreen,
    HomeScreen,
    PickConfigScreen,
    RunScreen,
)


class BenchApp(App):
    """Multi-screen TUI for ElMaestro.

    All screens are pushed/popped onto the app's screen stack. The initial
    screen is HomeScreen unless an initial config is passed in (in which
    case we jump straight to RunScreen).
    """

    TITLE = "ElMaestro"

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("ctrl+c", "quit", "Quit", show=False),
    ]

    def __init__(
        self,
        *,
        initial_config: Path | None = None,
        output_dir: Path | None = None,
    ) -> None:
        super().__init__()
        self.initial_config = initial_config
        self.output_dir = output_dir

    def on_mount(self) -> None:
        if self.initial_config is not None:
            self.push_screen(RunScreen(config=self.initial_config, output_dir=self.output_dir))
        else:
            self.push_screen(HomeScreen())


def run_home(*, output_dir: Path | None = None) -> None:
    """Open the TUI at the home menu. Default entry for `bench` with no args."""
    BenchApp(output_dir=output_dir).run()


def run_tui(config: Path, *, output_dir: Path | None = None) -> None:
    """Open the TUI straight to a config's Run screen."""
    BenchApp(initial_config=config, output_dir=output_dir).run()


__all__ = [
    "BenchApp",
    "BrowseResultsScreen",
    "CompareScreen",
    "HomeScreen",
    "PickConfigScreen",
    "RunScreen",
    "run_home",
    "run_tui",
]
