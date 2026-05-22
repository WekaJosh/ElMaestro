"""Typer CLI: bench {run, report, validate, version}."""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

import typer
import ulid
from rich.console import Console
from rich.table import Table

from . import __version__
from .config.loader import load_run_plan
from .config.sweep import materialize_run_refs
from .engine.coordinator import CoordinatorError, run as run_spec
from .report.compare import load_run, render_compare
from .report.render import render_single
from .results.schema import Result
from .results.store import (
    Manifest,
    new_run_dir,
    read_manifest,
    read_result,
    spec_dir,
    write_manifest,
    write_result,
)

app = typer.Typer(
    add_completion=False,
    no_args_is_help=True,
    help="Interactive IO benchmarking harness built on elbencho.",
)
console = Console()
err_console = Console(stderr=True, style="red")


@app.command()
def version() -> None:
    """Print the harness version."""
    console.print(f"elbencho-harness {__version__}")


@app.command()
def validate(config: Path = typer.Argument(..., exists=True, dir_okay=False)) -> None:
    """Load and validate a YAML config without running anything."""
    plan = load_run_plan(config)
    console.print(f"[green]✓[/green] {config} parses cleanly.")
    table = Table(show_header=True, header_style="bold")
    table.add_column("kind")
    table.add_column("name")
    table.add_column("detail")
    for t in plan.targets:
        if t.kind == "posix":
            table.add_row("posix", t.name, f"mount_path={t.mount_path} subdir={t.dataset_subdir}")
        else:
            table.add_row("s3", t.name, f"endpoint={t.endpoint} bucket={t.bucket}")
    for w in plan.workloads:
        table.add_row(
            "workload",
            w.name,
            f"bs={w.block_size}B rw_read={w.rw_mix_pct_read}% t={w.threads_per_client} qd={w.io_depth}",
        )
    for r in plan.runs:
        table.add_row("run", f"{r.target}/{r.workload}", "")
    for sw in plan.sweeps:
        targets = sw.targets or ([sw.target] if sw.target else [])
        table.add_row("sweep", sw.name, f"base={sw.base} targets={','.join(targets)}")
    console.print(table)


@app.command()
def run(
    config: Path = typer.Argument(..., exists=True, dir_okay=False),
    output_dir: Path = typer.Option(None, "--output-dir", "-o", help="Override output_dir from config"),
    timeout: int = typer.Option(None, "--timeout", help="Per-run subprocess timeout in seconds"),
    resume: Path = typer.Option(
        None,
        "--resume",
        help="Reuse an existing run directory; skip spec_hashes already marked completed.",
    ),
) -> None:
    """Execute the runs and sweeps in a config and render a report for each spec.

    Expansion order: plain `runs:` entries first (declaration order), then each
    sweep in declaration order. A sweep expands into one spec per axis combo
    (cartesian) or one per axis value (ladder); see docs/PLAN.md for details.
    """
    plan = load_run_plan(config)
    base_out = Path(output_dir or plan.output_dir).resolve()
    base_out.mkdir(parents=True, exist_ok=True)

    pairs = materialize_run_refs(plan)
    if not pairs:
        err_console.print("config has neither `runs:` nor `sweeps:` to execute")
        raise typer.Exit(code=2)

    # Pick a run label. Sweeps get their sweep name; plain runs use the first.
    first_point, first_spec = pairs[0]
    if first_point is not None:
        label = f"sweep_{first_point.sweep_name}"
    else:
        label = f"{first_spec.target.name}_{first_spec.workload.name}"

    # Resume: reuse run_dir and existing manifest if --resume points at one.
    if resume is not None:
        run_dir = resume.resolve()
        if not (run_dir / "manifest.json").is_file():
            err_console.print(f"resume target has no manifest.json: {run_dir}")
            raise typer.Exit(code=2)
        manifest = read_manifest(run_dir)
        console.print(f"[bold]Resuming:[/bold] {run_dir}")
    else:
        run_dir = new_run_dir(base_out, label)
        manifest = Manifest(run_id=ulid.new().str, created_at=datetime.utcnow())
        console.print(f"[bold]Run directory:[/bold] {run_dir}")

    completed_hashes = {h for h, s in manifest.statuses.items() if s == "completed"}

    for idx, (point, spec) in enumerate(pairs, start=1):
        sweep_label = point.short_label() if point is not None else None
        sd = spec_dir(run_dir, idx, spec.target.name, spec.workload.name, label=sweep_label)
        header = f"[{idx:04d}] {spec.target.name} · {spec.workload.name}"
        if sweep_label:
            header += f" · {sweep_label}"
        console.print(
            f"[bold cyan]→[/bold cyan] {header}  (spec_hash={spec.spec_hash[:18]}…)"
        )
        if spec.spec_hash in completed_hashes:
            console.print("  [dim]✓ already completed, skipping[/dim]")
            continue
        try:
            result: Result = run_spec(spec, spec_dir=sd, timeout_s=timeout)
        except CoordinatorError as e:
            err_console.print(f"  ✗ coordinator error: {e}")
            manifest.statuses[spec.spec_hash] = "error"
            continue

        write_result(sd, result)
        report_path = render_single(result, sd / "report.html")

        if result.elbencho_exit_code == 0:
            console.print(f"  [green]✓[/green] result.json written, report: {report_path}")
            manifest.statuses[spec.spec_hash] = "completed"
        else:
            err_console.print(
                f"  ✗ elbencho exited {result.elbencho_exit_code}; partial result written"
            )
            manifest.statuses[spec.spec_hash] = f"failed:{result.elbencho_exit_code}"

        # Update manifest after every spec so a SIGINT mid-sweep leaves resumable state.
        manifest.run_specs.append(
            {
                "index": idx,
                "spec_hash": spec.spec_hash,
                "run_id": spec.run_id,
                "target": spec.target.name,
                "workload": spec.workload.name,
                "sweep": point.sweep_name if point else None,
                "axis_values": point.overrides if point else None,
                "spec_dir": str(sd.relative_to(run_dir)),
            }
        )
        write_manifest(run_dir, manifest)

    write_manifest(run_dir, manifest)
    # Top-level pointer report links to the first completed run.
    if manifest.run_specs:
        try:
            first_spec_dir = run_dir / manifest.run_specs[0]["spec_dir"]
            res = read_result(first_spec_dir)
            render_single(res, run_dir / "report.html", run_label=label)
        except Exception:
            pass
    console.print(f"\n[bold green]Done.[/bold green] {run_dir}")


@app.command()
def expand(
    config: Path = typer.Argument(..., exists=True, dir_okay=False),
) -> None:
    """Dry-run: print every spec a `bench run` would execute, without running.

    Useful before kicking off a long sweep, especially to verify max_runs and
    ladder vs. cartesian counts match expectations.
    """
    plan = load_run_plan(config)
    pairs = materialize_run_refs(plan)
    if not pairs:
        err_console.print("config has neither `runs:` nor `sweeps:`")
        raise typer.Exit(code=2)
    table = Table(show_header=True, header_style="bold")
    table.add_column("#")
    table.add_column("source")
    table.add_column("target")
    table.add_column("workload")
    table.add_column("axes")
    table.add_column("clients")
    for idx, (point, spec) in enumerate(pairs, start=1):
        src = point.sweep_name if point else "runs"
        axes = point.short_label() if point else ""
        table.add_row(
            f"{idx:04d}",
            src,
            spec.target.name,
            spec.workload.name,
            axes,
            str(len(spec.clients)),
        )
    console.print(table)
    console.print(f"\n[bold]{len(pairs)}[/bold] spec(s) total.")


@app.command()
def report(
    results_dir: Path = typer.Argument(..., exists=True, file_okay=False),
) -> None:
    """(Re-)render HTML reports from existing result.json files."""
    rendered: list[Path] = []
    for child in sorted(results_dir.iterdir()):
        if not child.is_dir():
            continue
        rj = child / "result.json"
        if not rj.is_file():
            continue
        result = read_result(child)
        out = render_single(result, child / "report.html")
        rendered.append(out)
        console.print(f"  rendered {out}")
    if not rendered:
        err_console.print(f"no result.json files found under {results_dir}")
        raise typer.Exit(code=1)
    console.print(f"[bold green]Rendered[/bold green] {len(rendered)} report(s).")


@app.command()
def compare(
    run_dirs: list[Path] = typer.Argument(..., exists=True, file_okay=False),
    out: Path = typer.Option(
        Path("./compare.html"), "--out", "-o", help="Output HTML path"
    ),
    baseline: str = typer.Option(
        None,
        "--baseline",
        help="Label of the baseline run (default: first arg). Deltas are vs this run.",
    ),
    label: list[str] = typer.Option(
        None,
        "--label",
        help="Override run label(s). Pass once per run_dir, in order. "
        "Default: directory basename.",
    ),
    title: str = typer.Option(
        "elbencho-harness compare", "--title", help="Title shown in the report header"
    ),
) -> None:
    """Overlay N run directories into one comparison HTML report.

    Each positional argument is a run directory (the kind `bench run` produces).
    Specs are aligned across runs by (target, workload, sweep-axis-values). The
    diff table shows percentage change vs the baseline.
    """
    labels = list(label or [])
    if labels and len(labels) != len(run_dirs):
        err_console.print(
            f"--label count ({len(labels)}) must match run_dirs count ({len(run_dirs)})"
        )
        raise typer.Exit(code=2)
    loaded = [
        load_run(d, label=labels[i] if i < len(labels) else None)
        for i, d in enumerate(run_dirs)
    ]
    if any(len(lr.results) == 0 for lr in loaded):
        empties = [lr.label for lr in loaded if len(lr.results) == 0]
        err_console.print(
            f"these run directories have no parseable result.json files: {empties}"
        )
        raise typer.Exit(code=2)
    out_path = render_compare(loaded, out.resolve(), baseline_label=baseline, title=title)
    console.print(f"[bold green]Wrote[/bold green] {out_path}")


def main() -> None:
    app()


if __name__ == "__main__":
    main()
