"""Textual app: load a config, list specs, launch the run, show live progress.

Layout:
  +------- header ------+
  |  title / status     |
  +-- spec table -------+
  |  idx  target  ...   |
  |  status / time / Δ  |
  +-- footer / bindings +
  |  [r] run  [q] quit  |
  +---------------------+

Status updates flow from the worker thread to the UI via Textual `Message`s.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import ClassVar, Iterator

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.message import Message
from textual.widgets import DataTable, Footer, Header, Static

from . import runner as runner_mod
from .runner import (
    Event,
    RunFinished,
    RunStarted,
    SpecFinished,
    SpecPlanned,
    SpecStarted,
)


class _RunnerEvent(Message):
    """Wraps a runner.Event so the worker thread can post it to the app."""

    def __init__(self, event: Event) -> None:
        self.event = event
        super().__init__()


@dataclass
class _RowState:
    """What the UI knows about one spec. Mirrors the order columns appear."""

    target: str
    workload: str
    axes: str
    status: str = "queued"
    duration: str = ""


class BenchApp(App):
    CSS = """
    Screen { layout: vertical; }
    #status { padding: 1 2; color: $accent; }
    DataTable { height: 1fr; }
    """

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("r", "run_benchmark", "Run", show=True),
        Binding("q", "quit", "Quit", show=True),
        Binding("ctrl+c", "quit", "Quit", show=False),
    ]

    def __init__(self, config: Path, *, output_dir: Path | None = None) -> None:
        super().__init__()
        self.config = Path(config)
        self.output_dir = output_dir
        self._rows: list[_RowState] = []
        self._running = False

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static(f"Config: {self.config}", id="status")
        yield Vertical(DataTable(id="specs"))
        yield Footer()

    def on_mount(self) -> None:
        self.title = "elbencho-harness"
        self.sub_title = self.config.name
        table = self.query_one(DataTable)
        table.add_columns("#", "target", "workload", "axes", "status", "duration")
        self._load_plan()

    def _load_plan(self) -> None:
        """Populate the table from the config's expansion, without running."""
        try:
            _, pairs = runner_mod.plan_events(self.config)
        except Exception as e:
            self.query_one("#status", Static).update(f"[red]Failed to load: {e}[/red]")
            return
        table = self.query_one(DataTable)
        self._rows = []
        for idx, (point, spec) in enumerate(pairs, start=1):
            axes = point.short_label() if point else ""
            state = _RowState(target=spec.target.name, workload=spec.workload.name, axes=axes)
            self._rows.append(state)
            table.add_row(
                f"{idx:04d}",
                state.target,
                state.workload,
                state.axes,
                state.status,
                state.duration,
            )
        self.query_one("#status", Static).update(
            f"Loaded {len(self._rows)} spec(s) from {self.config}. Press [r] to run, [q] to quit."
        )

    def action_run_benchmark(self) -> None:
        if self._running:
            return
        self._running = True
        self.query_one("#status", Static).update("[yellow]Running…[/yellow]")
        # Background worker that walks the event iterator and posts each event.
        self.run_worker(self._worker, thread=True, exclusive=True)

    def _worker(self) -> None:
        """Runs in a background thread; posts events back to the UI."""
        try:
            for ev in runner_mod.execute(self.config, output_dir=self.output_dir):
                self.call_from_thread(self.post_message, _RunnerEvent(ev))
        except Exception as e:
            self.call_from_thread(self.post_message, _RunnerEvent(RunFinished(
                run_dir=Path("."), completed=0, failed=1,
            )))
            self.call_from_thread(
                self.query_one("#status", Static).update,
                f"[red]Worker crashed: {e}[/red]",
            )

    def on__runner_event(self, message: _RunnerEvent) -> None:  # noqa: D401 (Textual handler)
        """Handle a runner event posted from the worker."""
        ev = message.event
        if isinstance(ev, RunStarted):
            self.query_one("#status", Static).update(
                f"[yellow]Running {ev.total} spec(s) → {ev.run_dir}[/yellow]"
            )
        elif isinstance(ev, SpecPlanned):
            # Already in table from on_mount; nothing to do.
            pass
        elif isinstance(ev, SpecStarted):
            self._update_row(ev.index, status="running")
        elif isinstance(ev, SpecFinished):
            colored = self._color_status(ev.status)
            self._update_row(ev.index, status=colored, duration=f"{ev.duration_s:.1f}s")
        elif isinstance(ev, RunFinished):
            self._running = False
            self.query_one("#status", Static).update(
                f"[green]Done.[/green] completed={ev.completed} failed={ev.failed} "
                f"→ {ev.run_dir}"
            )

    def _update_row(self, idx: int, **changes: str) -> None:
        if not (1 <= idx <= len(self._rows)):
            return
        state = self._rows[idx - 1]
        for k, v in changes.items():
            setattr(state, k, v)
        table = self.query_one(DataTable)
        # DataTable rows are addressed by row index (0-based); columns by column key
        # but we use positional here for simplicity.
        row_index = idx - 1
        table.update_cell_at((row_index, 4), state.status)
        table.update_cell_at((row_index, 5), state.duration)

    def _color_status(self, status: str) -> str:
        if status == "completed":
            return "[green]completed[/green]"
        if status.startswith("failed"):
            return f"[red]{status}[/red]"
        if status == "error":
            return "[red]error[/red]"
        return status


def run_tui(config: Path, *, output_dir: Path | None = None) -> None:
    """CLI entrypoint. Blocks until the user quits."""
    app = BenchApp(config=config, output_dir=output_dir)
    app.run()


# Iterator alias for tests that want to drive the runner without the TUI.
def execute(config: Path) -> Iterator[Event]:
    yield from runner_mod.execute(config)
