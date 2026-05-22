"""Backend registry. Maps engine name -> Backend instance.

Configs select an engine via the top-level `engine:` YAML field. Default is
"elbencho" so any pre-v0.7 config continues to work unchanged.
"""

from __future__ import annotations

from .base import Backend, EngineVersion, TargetSupport
from .elbencho import ElbenchoBackend
from .fio import FioBackend

_REGISTRY: dict[str, Backend] = {
    ElbenchoBackend.name: ElbenchoBackend(),
    FioBackend.name: FioBackend(),
}


def get_backend(name: str) -> Backend:
    """Look up a backend by name. Raises KeyError with a helpful message."""
    try:
        return _REGISTRY[name]
    except KeyError:
        available = ", ".join(sorted(_REGISTRY))
        raise KeyError(
            f"unknown engine {name!r}; available: {available}"
        ) from None


def available_engines() -> list[str]:
    """Names of all registered backends. Stable order (sorted)."""
    return sorted(_REGISTRY)


__all__ = [
    "Backend",
    "EngineVersion",
    "TargetSupport",
    "get_backend",
    "available_engines",
]
