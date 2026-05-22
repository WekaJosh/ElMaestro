"""YAML -> RunPlan loader."""

from __future__ import annotations

from pathlib import Path

import yaml

from .models import RunPlan


def load_run_plan(path: Path | str) -> RunPlan:
    p = Path(path)
    if not p.is_file():
        raise FileNotFoundError(f"config not found: {p}")
    with p.open("r", encoding="utf-8") as fh:
        raw = yaml.safe_load(fh)
    if not isinstance(raw, dict):
        raise ValueError(f"{p}: top-level YAML must be a mapping, got {type(raw).__name__}")
    return RunPlan.model_validate(raw)
