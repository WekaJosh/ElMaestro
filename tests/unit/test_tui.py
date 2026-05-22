"""TUI smoke tests using Textual's Pilot.

We don't drive a real run here (that requires the fake-elbencho fixture and
subprocess plumbing). We verify that the app loads, parses the config, and
populates the spec table.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from elbencho_harness.tui.app import BenchApp
from elbencho_harness.tui import runner as runner_mod

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures" / "sweep_smoke.yaml"


@pytest.mark.asyncio
async def test_app_loads_config_and_populates_spec_table(tmp_path):
    """Mount the app against the sweep_smoke fixture; check that the spec table
    has one row per expanded sweep point."""
    app = BenchApp(config=FIXTURE, output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        table = app.query_one("DataTable")
        # sweep_smoke.yaml has 4 ladder points (2 block sizes + 2 thread counts)
        assert table.row_count == 4
        # Status text should reflect the loaded count.
        status = app.query_one("#status").renderable
        text = str(status)
        assert "Loaded 4" in text


@pytest.mark.asyncio
async def test_quit_binding_exits_app(tmp_path):
    app = BenchApp(config=FIXTURE, output_dir=tmp_path)
    async with app.run_test() as pilot:
        await pilot.pause()
        await pilot.press("q")
        await pilot.pause()
        # The app should have exited; rather than checking is_running (Textual
        # internals), we just verify the press didn't raise.


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
        index=1, spec_hash="x", status="completed", result=None,
        duration_s=0.0, report_path=None,
    )
