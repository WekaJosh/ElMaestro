"""Entry point for both `python -m elbencho_harness` and the PyInstaller binary.

Delegates to the same Typer app as the `bench` console script, so the binary
behaves identically: no args opens the TUI, subcommands stay available.
"""

from __future__ import annotations

from elbencho_harness.cli import main


if __name__ == "__main__":
    main()
