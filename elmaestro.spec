# PyInstaller spec for ElMaestro.
#
# Produces a single-file binary that bundles the Python interpreter + all
# Python deps. The user still needs `elbencho` and/or `fio` installed
# natively (those are external C binaries; we drive them via subprocess).
#
# Build:
#   .venv/bin/pyinstaller elmaestro.spec --clean --noconfirm
# Output:
#   dist/elmaestro    (single-file binary)
#
# Hidden imports are deps PyInstaller's static analysis misses. Textual loads
# CSS by dynamic import; asyncssh uses runtime crypto module discovery; etc.

from __future__ import annotations

from pathlib import Path

# Anchor to the spec file's location so this works from any cwd.
REPO_ROOT = Path(SPEC).resolve().parent  # noqa: F821 (SPEC injected by PyInstaller)


hidden_imports = [
    # Textual scans CSS files via importlib at runtime.
    "textual.css",
    "textual.widgets",
    "textual.widgets._directory_tree",
    "textual.widgets._checkbox",
    "textual.widgets._data_table",
    # asyncssh's optional crypto backends.
    "asyncssh.crypto",
    "cryptography.hazmat.backends.openssl",
    # Pydantic v2 runtime model rebuild path.
    "pydantic.deprecated.decorator",
    # Plotly's JSON encoder + figure factory lookups.
    "plotly.io._json",
    "plotly.io._html",
    "plotly.graph_objs",
    "plotly.graph_objects",
    # Our own backend modules (registry uses dynamic-ish imports).
    "elbencho_harness.backends.elbencho",
    "elbencho_harness.backends.fio",
]


# Data files: ship the Jinja templates with the binary so reports still render.
datas = [
    (str(REPO_ROOT / "src" / "elbencho_harness" / "report" / "templates"),
     "elbencho_harness/report/templates"),
]


a = Analysis(  # noqa: F821
    [str(REPO_ROOT / "src" / "elbencho_harness" / "__main__.py")],
    pathex=[str(REPO_ROOT / "src")],
    binaries=[],
    datas=datas,
    hiddenimports=hidden_imports,
    hookspath=[],
    runtime_hooks=[],
    excludes=[
        # Lighten the bundle. We don't ship dev tooling.
        "pytest",
        "ruff",
        "mypy",
        "IPython",
        "jupyter",
        "matplotlib",
        "tkinter",
    ],
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data)  # noqa: F821

exe = EXE(  # noqa: F821
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name="elmaestro",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,         # UPX can break macOS code signing; leave off
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,       # TUI lives in the terminal
    disable_windowed_traceback=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
