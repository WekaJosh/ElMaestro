"""Tests for the chart helpers: axis-value formatting, axis detection, toggle."""

from __future__ import annotations

import plotly.graph_objects as go
import pytest

from elbencho_harness.report.charts import (
    PALETTE,
    _human_bytes,
    axis_pretty_name,
    detect_common_axis,
    format_axis_value,
    overlay_with_toggle,
    sweep_overlay,
)


# --- _human_bytes -------------------------------------------------------------


@pytest.mark.parametrize(
    "n,expected",
    [
        (1024, "1k"),
        (65536, "64k"),
        (262144, "256k"),
        (1048576, "1m"),
        (4194304, "4m"),
        (16777216, "16m"),
        (1073741824, "1g"),
        (512, "512b"),
    ],
)
def test_human_bytes_canonical_sizes(n, expected):
    assert _human_bytes(n) == expected


def test_human_bytes_non_power_of_two_does_not_crash():
    # Real elbencho can emit odd byte sizes; we just want a sensible string.
    assert _human_bytes(1000) == "1000b"  # below 1k threshold
    out = _human_bytes(1500 * 1024)  # 1.5 MiB-ish
    assert "k" in out or "m" in out


# --- format_axis_value --------------------------------------------------------


def test_format_axis_value_block_size():
    assert format_axis_value("block_size", 65536) == "64k"
    assert format_axis_value("block_size", 1048576) == "1m"


def test_format_axis_value_dataset_size():
    assert format_axis_value("dataset_size", 1073741824) == "1g"


def test_format_axis_value_threads_is_plain_integer():
    assert format_axis_value("threads_per_client", 4) == "4"
    assert format_axis_value("threads_per_client", 16) == "16"


def test_format_axis_value_io_depth_is_plain_integer():
    assert format_axis_value("io_depth", 32) == "32"


def test_format_axis_value_rw_mix_has_percent_sign():
    assert format_axis_value("rw_mix_pct_read", 70) == "70%"


def test_format_axis_value_none_returns_empty_string():
    assert format_axis_value("threads_per_client", None) == ""


# --- axis_pretty_name ---------------------------------------------------------


def test_axis_pretty_name_known_axes():
    assert axis_pretty_name("block_size") == "block size"
    assert axis_pretty_name("threads_per_client") == "threads / client"
    assert axis_pretty_name("io_depth") == "IO depth"
    assert axis_pretty_name("rw_mix_pct_read") == "read mix"


def test_axis_pretty_name_unknown_returns_input():
    assert axis_pretty_name("custom_axis") == "custom_axis"


# --- detect_common_axis -------------------------------------------------------


def test_detect_common_axis_single_axis():
    axes = [{"block_size": 1024}, {"block_size": 4096}, {"block_size": 16384}]
    assert detect_common_axis(axes) == "block_size"


def test_detect_common_axis_returns_none_when_mixed():
    axes = [{"block_size": 1024}, {"threads_per_client": 4}]
    assert detect_common_axis(axes) is None


def test_detect_common_axis_returns_none_for_multi_key_dict():
    """A sweep point with multiple overrides isn't a 'common axis' case."""
    axes = [{"block_size": 1024, "threads_per_client": 4}]
    assert detect_common_axis(axes) is None


def test_detect_common_axis_returns_none_for_empty_dicts():
    assert detect_common_axis([{}, None]) is None


# --- sweep_overlay ------------------------------------------------------------


def test_sweep_overlay_bakes_both_bar_and_line_traces():
    """For N runs, the figure should have 2*N traces: N bars + N scatter."""
    fig = sweep_overlay(
        title="Test",
        x_labels=["64k", "256k", "1m"],
        x_axis_title="block size",
        y_title="MiB/s",
        series=[("run-A", [100, 200, 300]), ("run-B", [110, 210, 310])],
        hover_unit="MiB/s",
    )
    bar_traces = [t for t in fig.data if t.type == "bar"]
    scatter_traces = [t for t in fig.data if t.type == "scatter"]
    assert len(bar_traces) == 2
    assert len(scatter_traces) == 2


def test_sweep_overlay_initial_mode_bar_makes_bars_visible():
    fig = sweep_overlay(
        title="t", x_labels=["a"], x_axis_title="", y_title="",
        series=[("r", [1])],
        initial_mode="bar",
    )
    bar_traces = [t for t in fig.data if t.type == "bar"]
    scatter_traces = [t for t in fig.data if t.type == "scatter"]
    assert bar_traces[0].visible is True
    assert scatter_traces[0].visible is False


def test_sweep_overlay_initial_mode_line_makes_lines_visible():
    fig = sweep_overlay(
        title="t", x_labels=["a"], x_axis_title="", y_title="",
        series=[("r", [1])],
        initial_mode="line",
    )
    bar_traces = [t for t in fig.data if t.type == "bar"]
    scatter_traces = [t for t in fig.data if t.type == "scatter"]
    assert bar_traces[0].visible is False
    assert scatter_traces[0].visible is True


def test_sweep_overlay_has_no_plotly_updatemenus():
    """The Bar/Line toggle is rendered as HTML over the chart, not as a
    Plotly updatemenu. Plotly's button styling fights dark themes."""
    fig = sweep_overlay(
        title="t", x_labels=["a"], x_axis_title="", y_title="",
        series=[("r", [1])],
    )
    # In Plotly an absent updatemenus is an empty tuple, not None.
    assert not fig.layout.updatemenus


def test_overlay_with_toggle_includes_both_buttons():
    fig = sweep_overlay(
        title="t", x_labels=["a", "b"], x_axis_title="", y_title="",
        series=[("r", [1, 2])],
    )
    html = overlay_with_toggle(fig, n_series=1)
    assert 'data-mode="bar"' in html
    assert 'data-mode="line"' in html
    assert 'data-nseries="1"' in html
    assert 'class="chart-wrapper"' in html
    assert ">Bar</button>" in html
    assert ">Line</button>" in html


def test_overlay_with_toggle_marks_initial_mode_active():
    fig = sweep_overlay(
        title="t", x_labels=["a"], x_axis_title="", y_title="",
        series=[("r", [1])],
    )
    bar_initial = overlay_with_toggle(fig, n_series=1, initial_mode="bar")
    # The Bar button has class="active", the Line one doesn't.
    assert 'data-mode="bar" class="active"' in bar_initial
    assert 'data-mode="line" class=""' in bar_initial

    line_initial = overlay_with_toggle(fig, n_series=1, initial_mode="line")
    assert 'data-mode="bar" class=""' in line_initial
    assert 'data-mode="line" class="active"' in line_initial


def test_sweep_overlay_handles_none_values_without_crashing():
    """Real data has Nones for failed/missing specs."""
    fig = sweep_overlay(
        title="t", x_labels=["a", "b"], x_axis_title="", y_title="",
        series=[("r", [1.0, None])],
    )
    # No NaN in trace data; Nones get coerced to 0 for plotting.
    ys = list(fig.data[0].y)
    assert ys[1] == 0
