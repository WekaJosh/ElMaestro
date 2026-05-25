"""TUI smoke tests using Textual's Pilot.

We don't drive a real run here (that requires the fake-elbencho fixture and
subprocess plumbing). We verify that the app mounts, screens push/pop, and
the spec table populates from a real config.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from elbencho_harness.tui import runner as runner_mod
from elbencho_harness.tui.app import BenchApp
from elbencho_harness.tui.screens import HomeScreen, RunScreen

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "sweep_smoke.yaml"


@pytest.mark.asyncio
async def test_app_opens_home_screen_when_no_config(tmp_path):
    """`bench` no-args -> the TUI mounts HomeScreen, not RunScreen."""
    app = BenchApp(output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        assert isinstance(app.screen, HomeScreen)


@pytest.mark.asyncio
async def test_app_opens_run_screen_when_initial_config_given(tmp_path):
    """`bench tui <config>` -> the TUI jumps straight to RunScreen."""
    app = BenchApp(initial_config=FIXTURE, output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        assert isinstance(app.screen, RunScreen)


@pytest.mark.asyncio
async def test_run_screen_populates_spec_table(tmp_path):
    """RunScreen.on_mount expands the config and fills the DataTable."""
    app = BenchApp(initial_config=FIXTURE, output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        from textual.widgets import DataTable

        table = app.screen.query_one(DataTable)
        # sweep_smoke.yaml has 4 ladder points (2 block sizes + 2 thread counts)
        assert table.row_count == 4
        status = app.screen.query_one("#run-status").renderable
        assert "Loaded 4" in str(status)


@pytest.mark.asyncio
async def test_quit_from_home_exits_app(tmp_path):
    app = BenchApp(output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        await pilot.press("q")
        await pilot.pause()
        # The press must not have raised; success means we got here.


@pytest.mark.asyncio
async def test_escape_from_run_screen_goes_back_to_home(tmp_path):
    """Sanity: from RunScreen, escape pops back to HomeScreen on the stack.

    Skipped when initial_config is set since RunScreen is the bottom of the
    stack in that path.
    """
    app = BenchApp(output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        # Simulate user opening the "Run" button: click it.
        await pilot.click("#btn-run")
        await pilot.pause()
        # Now picking a config screen is on top; pop back via escape.
        await pilot.press("escape")
        await pilot.pause()
        assert isinstance(app.screen, HomeScreen)


def test_runner_plan_events_returns_expanded_pairs():
    """plan_events is pure / sync and is what the TUI uses to populate the table."""
    plan, pairs = runner_mod.plan_events(FIXTURE)
    assert len(plan.workloads) == 1
    assert len(pairs) == 4


def test_runner_event_dataclasses_are_constructible():
    """Cheap structural check that the event types stay public."""
    from datetime import datetime, timezone

    from elbencho_harness.tui.runner import (
        RunFinished,
        RunStarted,
        SpecFinished,
        SpecPlanned,
        SpecStarted,
    )

    assert SpecPlanned(index=1, target="t", workload="w", axis_label="", spec_hash="x")
    assert SpecStarted(index=1, spec_hash="x", started_at=datetime.now(timezone.utc))
    assert RunStarted(run_dir=Path("."), total=1)
    assert RunFinished(run_dir=Path("."), completed=1, failed=0)
    assert SpecFinished(
        index=1,
        spec_hash="x",
        status="completed",
        result=None,
        duration_s=0.0,
        report_path=None,
    )
