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
from .config.models import ClientHost, RunSpec
from .engine.coordinator import CoordinatorError, run as run_spec
from .report.render import render_single
from .results.schema import Result
from .results.store import (
    Manifest,
    new_run_dir,
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
) -> None:
    """Execute the runs defined in a config and render a report for each."""
    plan = load_run_plan(config)
    base_out = Path(output_dir or plan.output_dir).resolve()
    base_out.mkdir(parents=True, exist_ok=True)

    if not plan.runs:
        err_console.print("config has no `runs:` entries (v0.1 does not yet expand sweeps)")
        raise typer.Exit(code=2)

    label = f"{plan.runs[0].target}_{plan.runs[0].workload}"
    run_dir = new_run_dir(base_out, label)
    console.print(f"[bold]Run directory:[/bold] {run_dir}")

    manifest = Manifest(run_id=ulid.new().str, created_at=datetime.utcnow())

    for idx, ref in enumerate(plan.runs, start=1):
        target = plan.target_by_name(ref.target)
        workload = plan.workload_by_name(ref.workload)
        clients = plan.clients or [ClientHost()]
        spec = RunSpec(
            run_id=ulid.new().str,
            spec_hash=RunSpec.make_spec_hash(target, workload, clients),
            target=target,
            workload=workload,
            clients=clients,
        )
        sd = spec_dir(run_dir, idx, target.name, workload.name)
        console.print(
            f"[bold cyan]→[/bold cyan] [{idx:04d}] {target.name} · {workload.name}  "
            f"(spec_hash={spec.spec_hash[:18]}…)"
        )
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

        manifest.run_specs.append(
            {
                "index": idx,
                "spec_hash": spec.spec_hash,
                "run_id": spec.run_id,
                "target": target.name,
                "workload": workload.name,
                "spec_dir": str(sd.relative_to(run_dir)),
            }
        )

    write_manifest(run_dir, manifest)
    # Render a top-level pointer report that links to the first run (handy for sweeps later).
    if manifest.statuses:
        try:
            first_spec_dir = run_dir / manifest.run_specs[0]["spec_dir"]
            res = read_result(first_spec_dir)
            render_single(res, run_dir / "report.html", run_label=label)
        except Exception:
            pass
    console.print(f"\n[bold green]Done.[/bold green] {run_dir}")


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


def main() -> None:
    app()


if __name__ == "__main__":
    main()
