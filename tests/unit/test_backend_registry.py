"""Tests for the backends registry + Backend protocol conformance."""

from __future__ import annotations

import pytest

from elbencho_harness.backends import (
    Backend,
    available_engines,
    get_backend,
)
from elbencho_harness.backends.elbencho import ElbenchoBackend
from elbencho_harness.backends.fio import FioBackend


def test_registry_lists_both_backends():
    assert available_engines() == ["elbencho", "fio"]


def test_get_backend_returns_elbencho_instance():
    b = get_backend("elbencho")
    assert isinstance(b, ElbenchoBackend)


def test_get_backend_returns_fio_instance():
    b = get_backend("fio")
    assert isinstance(b, FioBackend)


def test_get_backend_unknown_raises_with_listing():
    with pytest.raises(KeyError, match="elbencho, fio"):
        get_backend("nonexistent")


def test_both_backends_conform_to_protocol():
    """Sanity: every registered backend has the methods the coordinator calls."""
    for name in available_engines():
        b = get_backend(name)
        assert isinstance(b, Backend)
        assert b.name == name
        # Methods exist (we don't call them; that's covered by per-backend tests).
        for method in (
            "detect_version",
            "build_argv",
            "parse_results",
            "supports_target",
            "service_command",
        ):
            assert callable(getattr(b, method)), f"{name} missing {method}"
