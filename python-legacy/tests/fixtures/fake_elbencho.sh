#!/usr/bin/env bash
# A minimal elbencho stub used for testing the harness without the real binary.
# It accepts the flags the harness emits, writes plausible CSV / JSON output,
# and exits 0.
set -euo pipefail

if [[ "${1:-}" == "--version" ]]; then
  cat <<EOF
elbencho version: 3.1.3
Built with features: S3
EOF
  exit 0
fi

CSV=""
JSON=""
RESFILE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --csvfile) CSV="$2"; shift 2;;
    --jsonfile) JSON="$2"; shift 2;;
    --resfile) RESFILE="$2"; shift 2;;
    *) shift;;
  esac
done

mkdir -p "$(dirname "$CSV")"

cat > "$CSV" <<'CSV'
ISO date,label,operation,threads,block size,IOPS [first],IOPS [last],MiB/s [first],MiB/s [last],entries [last],MiB [last],CPU% [last],IO lat us min,IO lat us avg,IO lat us max,Ent lat us min,Ent lat us avg,Ent lat us max
2026-05-22T14:03:11Z,bench,WRITE,4,1048576,820,790,820.5,790.1,4,1024,38.4,210,1200,8200,210,1200,8200
2026-05-22T14:03:25Z,bench,READ,4,1048576,18432,17900,18432.1,17900.0,4,1024,42.1,12,612,7800,12,612,7800
CSV

cat > "$JSON" <<'JSON'
{
  "version": "3.1.3",
  "phases": [
    {
      "operation": "WRITE",
      "iops": 820,
      "mibps": 820.5,
      "latency": {"p50": 800, "p90": 1500, "p99": 2400, "p999": 5100}
    },
    {
      "operation": "READ",
      "iops": 18432,
      "mibps": 18432.1,
      "latency": {"p50": 480, "p90": 920, "p99": 1500, "p999": 3100}
    }
  ]
}
JSON

if [[ -n "$RESFILE" ]]; then
  echo "fake elbencho summary written" > "$RESFILE"
fi

echo "fake elbencho run complete (CSV=$CSV)"
exit 0
