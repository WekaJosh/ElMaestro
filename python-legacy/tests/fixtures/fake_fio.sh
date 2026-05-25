#!/usr/bin/env bash
# Minimal fio stub used for testing the harness without the real binary.
# Accepts the flags the harness emits, writes a plausible fio JSON output
# at --output=PATH, and exits 0.
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  echo "fio-3.36"
  exit 0
fi

OUTPUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output=*) OUTPUT="${1#--output=}"; shift;;
    --output) OUTPUT="$2"; shift 2;;
    *) shift;;
  esac
done

if [[ -z "$OUTPUT" ]]; then
  echo "fake_fio: --output is required" >&2
  exit 2
fi

mkdir -p "$(dirname "$OUTPUT")"

# Plausible fio --output-format=json structure. Fields match what
# FioBackend._phases_from_fio_json reads.
cat > "$OUTPUT" <<'JSON'
{
  "fio version": "fio-3.36",
  "jobs": [
    {
      "jobname": "bench",
      "groupid": 0,
      "error": 0,
      "read": {
        "io_bytes": 1073741824,
        "bw": 9216000,
        "iops": 9000,
        "clat_ns": {
          "min": 12000,
          "max": 7800000,
          "mean": 612000,
          "percentile": {
            "50.000000": 480000,
            "90.000000": 920000,
            "99.000000": 1500000,
            "99.900000": 3100000
          }
        }
      },
      "write": {
        "io_bytes": 0,
        "bw": 0,
        "iops": 0,
        "clat_ns": {"min": 0, "max": 0, "mean": 0}
      }
    }
  ]
}
JSON

echo "fake fio run complete (OUTPUT=$OUTPUT)"
exit 0
