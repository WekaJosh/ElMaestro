"""YAML -> RunPlan loader.

Supports a small set of substitution placeholders so configs are portable:

  ${CONFIG_DIR}  resolves to the absolute directory containing the YAML file.
                 Use this when a config references a sibling file (e.g. a
                 fake-elbencho fixture in the same directory) so it works on
                 any checkout location.
  ${HOME}        resolves to the user's home directory.
  $ENV{NAME}     resolves to the value of the named environment variable, or
                 an empty string if unset. Useful for credentials_ref paths
                 and per-host data dirs.

Substitution is purely textual and happens before YAML parsing, so it works
for any string-valued field (paths, hosts, prefixes, etc.).
"""

from __future__ import annotations

import os
import re
from pathlib import Path

import yaml

from .models import RunPlan

_ENV_RE = re.compile(r"\$ENV\{([A-Za-z_][A-Za-z0-9_]*)\}")


def _expand_placeholders(raw: str, *, config_dir: Path) -> str:
    """Replace ${CONFIG_DIR}, ${HOME}, and $ENV{NAME} in a raw YAML string."""
    out = raw.replace("${CONFIG_DIR}", str(config_dir))
    out = out.replace("${HOME}", str(Path.home()))
    out = _ENV_RE.sub(lambda m: os.environ.get(m.group(1), ""), out)
    return out


def load_run_plan(path: Path | str) -> RunPlan:
    p = Path(path)
    if not p.is_file():
        raise FileNotFoundError(f"config not found: {p}")
    raw_text = p.read_text(encoding="utf-8")
    expanded = _expand_placeholders(raw_text, config_dir=p.resolve().parent)
    parsed = yaml.safe_load(expanded)
    if not isinstance(parsed, dict):
        raise ValueError(f"{p}: top-level YAML must be a mapping, got {type(parsed).__name__}")
    return RunPlan.model_validate(parsed)
