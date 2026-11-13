#!/usr/bin/env bash
# End-to-end check: decode a recorded I/Q file with the real binary and exercise every network output.
# Usage: scripts/e2e.sh [path-to-adsbpi-binary]
set -u
cd "$(dirname "$0")/.."
BIN=${1:-./target/release/adsbpi}
TMP=$(mktemp -d)
PID=
trap '[ -n "$PID" ] && kill $PID 2>/dev/null; rm -rf "$TMP"' EXIT
fail=0
check() { if eval "$2"; then echo "PASS  $1"; else echo "FAIL  $1"; fail=1; fi; }

cfg() { printf 'state_dir = "%s/%s"\n' "$TMP" "$1" > "$TMP/$1.toml"; }

echo "== file decode (no home set)"
cfg s1
SUMMARY=$("$BIN" --config "$TMP/s1.toml" --file testdata/modes1.bin 2>/dev/null)
echo "$SUMMARY" | head -4
MSGS=$(echo "$SUMMARY" | head -1 | awk '{print $1}')
check "decoded messages from the real recording" "[ \"${MSGS:-0}\" -gt 20 ]"
check "positions resolved when home is unset" "echo '$SUMMARY' | head -1 | grep -qE ', [1-9][0-9]* positions'"

echo "== range check"
cfg s2
mkdir -p "$TMP/s2"
echo '{"lat":32.9618,"lon":-96.8292}' > "$TMP/s2/home.json"
FAR=$("$BIN" --config "$TMP/s2.toml" --file testdata/modes1.bin 2>/dev/null | head -1)
echo "$FAR"
check "Malta traffic rejected for a Texas home" "echo '$FAR' | grep -q ', 0 positions'"

echo "== web + API"
cfg s3
"$BIN" --config "$TMP/s3.toml" --file testdata/modes1.bin --serve \
  --web-bind 127.0.0.1:18080 --beast-bind 127.0.0.1:13005 --sbs-bind 127.0.0.1:13003 >"$TMP/log" 2>&1 &
PID=$!
sleep 2
W=http://127.0.0.1:18080
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
check "GET / serves the map page" "[ \"\$(code $W/)\" = 200 ]"
check "GET /leaflet/leaflet.js embedded" "[ \"\$(code $W/leaflet/leaflet.js)\" = 200 ]"
check "unknown path is 404" "[ \"\$(code $W/nope)\" = 404 ]"
check "aircraft.json lists aircraft" "curl -s $W/data/aircraft.json | grep -q '\"hex\"'"
check "status reports messages" "curl -s $W/api/status | grep -q '\"messages\"'"
check "placeholder home -1,-1 rejected" "[ \"\$(code -X POST -d '{\"lat\":-1,\"lon\":-1}' $W/api/home)\" = 400 ]"
check "garbage home body rejected" "[ \"\$(code -X POST -d nope $W/api/home)\" = 400 ]"
check "valid home accepted + persisted" "curl -s -X POST -d '{\"lat\":32.9618,\"lon\":-96.8292}' $W/api/home | grep -q '\"persisted\":true' && grep -q 32.9618 $TMP/s3/home.json"
check "status shows the new home" "curl -s $W/api/status | grep -q '\"lat\":32.9618'"
kill $PID 2>/dev/null; wait $PID 2>/dev/null; PID=

echo "== live Beast / SBS streams (clients connect first, then the recording is replayed)"
cfg s4
"$BIN" --config "$TMP/s4.toml" --file testdata/modes1.bin --serve --start-delay-ms 1500 \
  --web-bind 127.0.0.1:18081 --beast-bind 127.0.0.1:13015 --sbs-bind 127.0.0.1:13013 >"$TMP/log2" 2>&1 &
PID=$!
sleep 0.5
timeout 4 bash -c 'cat < /dev/tcp/127.0.0.1/13015 > "$0"' "$TMP/beast.bin" &
B=$!
timeout 4 bash -c 'cat < /dev/tcp/127.0.0.1/13013 > "$0"' "$TMP/sbs.txt" &
S=$!
wait $B $S 2>/dev/null
BEAST_BYTES=$(wc -c < "$TMP/beast.bin")
check "Beast stream carries data ($BEAST_BYTES bytes)" "[ $BEAST_BYTES -gt 500 ]"
check "Beast frames start with 0x1A and type 2/3" "head -c 2 $TMP/beast.bin | od -An -tx1 | grep -qE '^ 1a (32|33)'"
check "SBS stream has MSG lines" "grep -q '^MSG,' $TMP/sbs.txt"
check "SBS has callsign AMC421" "grep -q 'AMC421' $TMP/sbs.txt"
check "SBS has a position line (MSG,3 with lat/lon)" "grep -qE '^MSG,3,.*,3[0-9][.][0-9]+,1[0-9][.][0-9]+,' $TMP/sbs.txt"
check "every SBS line has 22 fields" "! awk -F, 'NF!=22' $TMP/sbs.txt | grep -q ."

[ $fail = 0 ] && echo "ALL PASSED" || { echo "SOME FAILED"; echo "--- server log"; cat "$TMP/log"; }
exit $fail
