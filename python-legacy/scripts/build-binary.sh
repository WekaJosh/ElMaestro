#!/usr/bin/env bash
# Build a single-file ElMaestro binary using PyInstaller.
#
# Output: dist/elmaestro-<os>-<arch>
# Requires: a venv with the project installed (pip install -e ".[dev,ssh,tui]")
#           plus pyinstaller (pip install pyinstaller).
#
# Usage:
#   .venv/bin/pip install pyinstaller
#   scripts/build-binary.sh
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

if [[ ! -x .venv/bin/pyinstaller ]]; then
  echo "error: .venv/bin/pyinstaller not found. Install with: .venv/bin/pip install pyinstaller" >&2
  exit 2
fi

# Detect os + arch for the output filename.
OS_NAME="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$OS_NAME" in
  darwin) OS_TAG="macos" ;;
  linux)  OS_TAG="linux" ;;
  *)      OS_TAG="$OS_NAME" ;;
esac

echo ">>> Building elmaestro for ${OS_TAG}-${ARCH}"
.venv/bin/pyinstaller elmaestro.spec --clean --noconfirm

TARGET="dist/elmaestro-${OS_TAG}-${ARCH}"
mv -f dist/elmaestro "$TARGET"
chmod +x "$TARGET"

SIZE=$(du -h "$TARGET" | cut -f1)
echo ""
echo ">>> Built $TARGET (${SIZE})"
echo ">>> Quick sanity check:"
"$TARGET" version
echo ""
echo "Binary is self-contained except for the engine binaries (elbencho / fio),"
echo "which must be installed on every machine that actually runs IO."
