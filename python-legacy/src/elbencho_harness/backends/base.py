"""Backend protocol: the contract every benchmark engine implements.

A backend translates a RunSpec into a concrete subprocess invocation and
parses the output back into the canonical PhaseResult schema. The
coordinator stays engine-agnostic above this line.

Engines covered so far:
  - elbencho (the original, full POSIX + S3 + service-mode fan-out)
  - fio     (POSIX, client/server fan-out; S3 deferred)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import ClassVar, Protocol, runtime_checkable

from ..config.models import ClientHost, RunSpec, Target
from ..results.schema import EngineArtifactRefs, PhaseResult


@dataclass
class EngineVersion:
    """Parsed output of `<binary> --version`. Generic across engines."""

    raw: str
    version: str | None
    features: list[str] = field(default_factory=list)

    def has(self, feature: str) -> bool:
        return any(feature.lower() == f.lower() for f in self.features)


@dataclass
class TargetSupport:
    """Whether a backend supports a given target. `reason` populated when not."""

    supported: bool
    reason: str = ""


@runtime_checkable
class Backend(Protocol):
    """A benchmark engine the harness can drive.

    Implementations live in sibling modules and register themselves via
    backends/__init__.py.
    """

    name: ClassVar[str]
    "Engine name as it appears in YAML's top-level `engine:` field."

    def detect_version(self, local_path: str) -> EngineVersion:
        """Run `<binary> --version` on the local machine and parse output."""

    def build_argv(
        self,
        spec: RunSpec,
        raw_dir: Path,
        *,
        local_path: str,
        hosts: str | None = None,
    ) -> tuple[list[str], str]:
        """Construct the engine command for one RunSpec.

        Args:
          spec:        the resolved spec (target, workload, clients).
          raw_dir:     directory where the engine writes its output files.
                       Must already exist.
          local_path:  path to the binary on the local (coordinator) machine.
          hosts:       optional comma-separated host:port list for multi-client
                       runs. If None, the backend builds a single-host command.

        Returns:
          (argv, primary_phase) where primary_phase is the phase whose numbers
          are the headline in the report (read | write | mixed).
        """

    def parse_results(
        self, raw_dir: Path, *, command: str
    ) -> tuple[dict[str, PhaseResult], EngineArtifactRefs]:
        """Parse the engine's output files in raw_dir.

        Returns:
          phases dict (keyed by 'read' | 'write' | 'mixed' | ...): non-IO
            phases must be excluded; raw artifacts stay on disk.
          EngineArtifactRefs: paths the engine wrote to + the command line.
        """

    def supports_target(self, target: Target) -> TargetSupport:
        """Whether this backend can drive the given target.

        Used by the coordinator to fail-fast with a clear message before
        starting subprocesses. e.g. fio doesn't yet support S3 targets, so
        FioBackend returns supported=False, reason="fio S3 support is
        roadmap; use the elbencho engine for S3".
        """

    def service_command(self, client: ClientHost) -> list[str]:
        """argv to start the engine's service/server mode on a remote host.

        For elbencho: ["<path>", "--service", "--port", "<P>"].
        For fio:      ["<path>", "--server=,N:<P>"] (port-only bind).
        """
