#!/usr/bin/env bash
# Rough decode-throughput check: replays the test recording N times and reports speed vs real time.
# The radio produces 4 MB/s (2 MS/s x 2 bytes), so anything well above 1.0x keeps up in real time.
# Usage: scripts/bench.sh [binary] [repeats]
set -eu
cd "$(dirname "$0")/.."
BIN=${1:-./target/release/adsbpi}
N=${2:-150}
T=$(mktemp)
trap 'rm -f "$T"' EXIT
for _ in $(seq "$N"); do cat testdata/modes1.bin; done > "$T"
BYTES=$(stat -c %s "$T")
START=$(date +%s.%N)
"$BIN" --file "$T" >/dev/null 2>&1
END=$(date +%s.%N)
awk -v b="$BYTES" -v s="$START" -v e="$END" 'BEGIN {
  secs = e - s; radio = b / 4e6;
  printf "%.1f s of radio decoded in %.2f s  ->  %.0fx real time\n", radio, secs, radio / secs }'
