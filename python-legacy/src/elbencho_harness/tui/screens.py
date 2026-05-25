"""Textual screens for the TUI.

Screens (push/pop navigation):
  - HomeScreen           main menu: Run / Browse / Compare / Quit
  - PickConfigScreen     file picker for a YAML config; calls back via callback
  - RunScreen            shows expanded specs, launches the run, live status
  - BrowseResultsScreen  pick a results/ dir and open report.html in browser
  - CompareScreen        multi-select run dirs, render compare HTML
"""

from __future__ import annotations

import subprocess
import sys
import webbrowser
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, ClassVar

from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Container, Horizontal, Vertical, VerticalScroll
from textual.message import Message
from textual.screen import Screen
from textual.widgets import (
    Button,
    Checkbox,
    DataTable,
    DirectoryTree,
    Footer,
    Header,
    Input,
    Static,
)

from . import runner as runner_mod
from .runner import (
    Event,
    RunFinished,
    RunStarted,
    SpecFinished,
    SpecPlanned,
    SpecStarted,
)


# ---------------------------------------------------------------------------
# Home
# ---------------------------------------------------------------------------


class HomeScreen(Screen):
    """Top-level menu. Single column of large buttons; keyboard-first."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("q", "quit", "Quit", show=True),
        Binding("ctrl+c", "quit", "Quit", show=False),
    ]

    CSS = """
    #home-wrap {
      align: center middle;
      height: 1fr;
    }
    #home-card {
      width: 60;
      border: round $primary;
      padding: 2 4;
    }
    #home-title {
      text-style: bold;
      content-align: center middle;
      margin-bottom: 1;
    }
    #home-sub {
      color: $text-muted;
      content-align: center middle;
      margin-bottom: 2;
    }
    Button {
      width: 100%;
      margin-bottom: 1;
    }
    """

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        with Container(id="home-wrap"):
            with Vertical(id="home-card"):
                yield Static("ElMaestro", id="home-title")
                yield Static("IO benchmarking harness · elbencho + fio", id="home-sub")
                yield Button("Run a benchmark", id="btn-run", variant="primary")
                yield Button("Browse past results", id="btn-browse")
                yield Button("Compare runs", id="btn-compare")
                yield Button("Quit", id="btn-quit")
        yield Footer()

    def on_mount(self) -> None:
        self.app.title = "ElMaestro"
        self.app.sub_title = "home"

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "btn-run":
            self.app.push_screen(
                PickConfigScreen(
                    title="Pick a config to run",
                    on_pick=self._open_run,
                )
            )
        elif event.button.id == "btn-browse":
            self.app.push_screen(BrowseResultsScreen())
        elif event.button.id == "btn-compare":
            self.app.push_screen(CompareScreen())
        elif event.button.id == "btn-quit":
            self.app.exit()

    def _open_run(self, config: Path) -> None:
        self.app.push_screen(RunScreen(config=config))


# ---------------------------------------------------------------------------
# File picker
# ---------------------------------------------------------------------------


class _YamlOnlyTree(DirectoryTree):
    """DirectoryTree that hides everything except dirs and YAML files."""

    def filter_paths(self, paths):
        for p in paths:
            if p.is_dir() and not p.name.startswith("."):
                yield p
            elif p.suffix in {".yaml", ".yml"}:
                yield p


class PickConfigScreen(Screen):
    """File picker rooted at cwd. Calls `on_pick(path)` when the user picks a YAML."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("escape", "app.pop_screen", "Back", show=True),
        Binding("q", "app.pop_screen", "Back", show=False),
    ]

    CSS = """
    #picker-title { padding: 1 2; color: $accent; }
    #picker-path  { padding: 0 2 1 2; color: $text-muted; }
    DirectoryTree { height: 1fr; }
    """

    def __init__(
        self,
        *,
        title: str = "Pick a file",
        on_pick: Callable[[Path], None] | None = None,
        start_dir: Path | None = None,
    ) -> None:
        super().__init__()
        self.title_text = title
        self.on_pick = on_pick
        self.start_dir = Path(start_dir or Path.cwd()).resolve()

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static(self.title_text, id="picker-title")
        yield Static(f"Start: {self.start_dir}", id="picker-path")
        yield _YamlOnlyTree(str(self.start_dir))
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = "pick config"
        self.query_one(DirectoryTree).focus()

    def on_directory_tree_file_selected(self, event: DirectoryTree.FileSelected) -> None:
        path = Path(event.path)
        if self.on_pick is not None:
            cb = self.on_pick
            self.app.pop_screen()
            cb(path)


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------


class _RunnerEvent(Message):
    """Wraps a runner.Event so the worker thread can post it to the screen."""

    def __init__(self, event: Event) -> None:
        self.event = event
        super().__init__()


@dataclass
class _RowState:
    target: str
    workload: str
    axes: str
    status: str = "queued"
    duration: str = ""


class RunScreen(Screen):
    """Show the expanded spec list for a config, launch the run, watch live."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("r", "run_benchmark", "Run", show=True),
        Binding("escape", "app.pop_screen", "Back", show=True),
        Binding("q", "app.pop_screen", "Back", show=False),
    ]

    CSS = """
    #run-status { padding: 1 2; color: $accent; }
    DataTable { height: 1fr; }
    """

    def __init__(self, *, config: Path, output_dir: Path | None = None) -> None:
        super().__init__()
        self.config = Path(config)
        self.output_dir = output_dir
        self._rows: list[_RowState] = []
        self._running = False

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static(f"Config: {self.config}", id="run-status")
        yield Vertical(DataTable(id="specs"))
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = self.config.name
        table = self.query_one(DataTable)
        table.add_columns("#", "target", "workload", "axes", "status", "duration")
        self._load_plan()

    def _load_plan(self) -> None:
        try:
            _, pairs = runner_mod.plan_events(self.config)
        except Exception as e:
            self.query_one("#run-status", Static).update(f"[red]Failed to load: {e}[/red]")
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
        self.query_one("#run-status", Static).update(
            f"Loaded {len(self._rows)} spec(s). Press [r] to run, [esc] to go back."
        )

    def action_run_benchmark(self) -> None:
        if self._running:
            return
        self._running = True
        self.query_one("#run-status", Static).update("[yellow]Running…[/yellow]")
        self.app.run_worker(self._worker, thread=True, exclusive=True)

    def _worker(self) -> None:
        try:
            for ev in runner_mod.execute(self.config, output_dir=self.output_dir):
                self.app.call_from_thread(self.post_message, _RunnerEvent(ev))
        except Exception as e:
            self.app.call_from_thread(
                self.post_message,
                _RunnerEvent(RunFinished(run_dir=Path("."), completed=0, failed=1)),
            )
            self.app.call_from_thread(
                self.query_one("#run-status", Static).update,
                f"[red]Worker crashed: {e}[/red]",
            )

    def on__runner_event(self, message: _RunnerEvent) -> None:  # noqa: D401
        ev = message.event
        if isinstance(ev, RunStarted):
            self.query_one("#run-status", Static).update(
                f"[yellow]Running {ev.total} spec(s) → {ev.run_dir}[/yellow]"
            )
        elif isinstance(ev, SpecPlanned):
            pass
        elif isinstance(ev, SpecStarted):
            self._update_row(ev.index, status="running")
        elif isinstance(ev, SpecFinished):
            self._update_row(
                ev.index, status=self._color_status(ev.status), duration=f"{ev.duration_s:.1f}s"
            )
        elif isinstance(ev, RunFinished):
            self._running = False
            self.query_one("#run-status", Static).update(
                f"[green]Done.[/green] completed={ev.completed} failed={ev.failed} → {ev.run_dir}"
            )

    def _update_row(self, idx: int, **changes: str) -> None:
        if not (1 <= idx <= len(self._rows)):
            return
        state = self._rows[idx - 1]
        for k, v in changes.items():
            setattr(state, k, v)
        table = self.query_one(DataTable)
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


# ---------------------------------------------------------------------------
# Browse results
# ---------------------------------------------------------------------------


def _open_in_browser(path: Path) -> None:
    """Open a local file in the user's default browser. Best-effort, never raises."""
    try:
        webbrowser.open(path.resolve().as_uri())
    except Exception:
        # Fallback for systems where webbrowser doesn't work over SSH/headless.
        try:
            opener = "open" if sys.platform == "darwin" else "xdg-open"
            subprocess.Popen([opener, str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            pass


class BrowseResultsScreen(Screen):
    """List recent run directories under ./results/, open report.html on Enter."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("enter", "open_selected", "Open report", show=True),
        Binding("escape", "app.pop_screen", "Back", show=True),
        Binding("q", "app.pop_screen", "Back", show=False),
    ]

    CSS = """
    #browse-title { padding: 1 2; color: $accent; }
    #browse-path  { padding: 0 2 1 2; color: $text-muted; }
    DataTable { height: 1fr; }
    """

    def __init__(self, results_root: Path | None = None) -> None:
        super().__init__()
        self.results_root = Path(results_root or Path.cwd() / "results").resolve()

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static("Browse past runs", id="browse-title")
        yield Static(f"Searching: {self.results_root}", id="browse-path")
        yield DataTable(id="runs")
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = "browse"
        table = self.query_one(DataTable)
        table.cursor_type = "row"
        table.add_columns("run dir", "specs", "engine")
        if not self.results_root.is_dir():
            return
        runs = sorted(
            (p for p in self.results_root.iterdir() if p.is_dir()),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        for run_dir in runs[:50]:
            specs = sum(1 for c in run_dir.iterdir() if c.is_dir() and (c / "result.json").is_file())
            engine = self._sniff_engine(run_dir)
            table.add_row(run_dir.name, str(specs), engine)
        table.focus()

    def _sniff_engine(self, run_dir: Path) -> str:
        """Read one result.json to find which engine produced this run."""
        import json

        for c in sorted(run_dir.iterdir()):
            rj = c / "result.json"
            if rj.is_file():
                try:
                    return json.loads(rj.read_text(encoding="utf-8")).get("engine", "?")
                except Exception:
                    return "?"
        return "?"

    def action_open_selected(self) -> None:
        table = self.query_one(DataTable)
        if not table.row_count:
            return
        row_idx = table.cursor_row
        name = table.get_cell_at((row_idx, 0))
        report = self.results_root / name / "report.html"
        if not report.is_file():
            # Fall back to the first spec's report.
            for c in sorted((self.results_root / name).iterdir()):
                if c.is_dir():
                    candidate = c / "report.html"
                    if candidate.is_file():
                        report = candidate
                        break
        if report.is_file():
            _open_in_browser(report)


# ---------------------------------------------------------------------------
# Compare
# ---------------------------------------------------------------------------


class CompareScreen(Screen):
    """Multi-select recent runs, render a compare HTML, open in browser."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("c", "render_compare", "Compare", show=True),
        Binding("escape", "app.pop_screen", "Back", show=True),
        Binding("q", "app.pop_screen", "Back", show=False),
    ]

    CSS = """
    #compare-title { padding: 1 2; color: $accent; }
    #compare-hint  { padding: 0 2 1 2; color: $text-muted; }
    Checkbox { padding: 0 2; }
    #compare-list { height: 1fr; }
    """

    def __init__(self, results_root: Path | None = None) -> None:
        super().__init__()
        self.results_root = Path(results_root or Path.cwd() / "results").resolve()

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static("Compare runs", id="compare-title")
        yield Static(
            "Check 2+ runs (space toggles), then press [c] to render.",
            id="compare-hint",
        )
        with VerticalScroll(id="compare-list"):
            if self.results_root.is_dir():
                runs = sorted(
                    (p for p in self.results_root.iterdir() if p.is_dir()),
                    key=lambda p: p.stat().st_mtime,
                    reverse=True,
                )[:30]
                for run_dir in runs:
                    yield Checkbox(run_dir.name, id=f"chk-{run_dir.name}", value=False)
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = "compare"

    def action_render_compare(self) -> None:
        picked: list[Path] = []
        for c in self.query(Checkbox):
            if c.value:
                name = c.id.removeprefix("chk-")
                picked.append(self.results_root / name)
        if len(picked) < 2:
            self.query_one("#compare-hint", Static).update(
                "[red]Pick at least 2 runs.[/red]"
            )
            return
        try:
            from ..report.compare import load_run, render_compare

            loaded = [load_run(p) for p in picked]
            out = self.results_root / f"compare-{loaded[0].label}-vs-{loaded[-1].label}.html"
            render_compare(loaded, out)
            _open_in_browser(out)
            self.query_one("#compare-hint", Static).update(f"[green]Wrote {out}[/green]")
        except Exception as e:
            self.query_one("#compare-hint", Static).update(f"[red]Compare failed: {e}[/red]")
