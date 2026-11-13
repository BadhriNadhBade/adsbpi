#!/bin/sh
# Exercise install.sh in staging mode (DESTDIR): nothing on the real system is touched.
# Usage: scripts/test-install.sh [path-to-adsbpi-binary]     (run with dash/sh, not as root-required)
set -u
cd "$(dirname "$0")/.."
BIN=${1:-./target/release/adsbpi}
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
fail=0
check() { if eval "$2"; then echo "PASS  $1"; else echo "FAIL  $1"; fail=1; fi; }
run() { DESTDIR="$ROOT" ADSBPI_LOCAL_BIN="$BIN" sh ./install.sh "$@"; }

echo "== install with a home position"
run_out=$(ADSBPI_LAT=32.9618 ADSBPI_LON=-96.8292 run 2>&1); echo "$run_out" | tail -3
check "binary installed and executable" "[ -x $ROOT/usr/local/bin/adsbpi ]"
check "systemd unit written" "grep -q 'ExecStart=/usr/local/bin/adsbpi --config /etc/adsbpi.toml' $ROOT/etc/systemd/system/adsbpi.service"
check "unit runs as unprivileged user" "grep -q '^User=adsbpi' $ROOT/etc/systemd/system/adsbpi.service"
check "udev rule covers 0bda:2838" "grep -q 'idProduct}==\"2838\"' $ROOT/etc/udev/rules.d/60-adsbpi-rtlsdr.rules"
check "DVB driver blacklisted" "grep -q 'blacklist dvb_usb_rtl28xxu' $ROOT/etc/modprobe.d/adsbpi-blacklist-rtl.conf"
check "config contains the home position" "grep -q 'lat = 32.9618' $ROOT/etc/adsbpi.toml"
check "generated config is accepted by the binary" "$BIN --config $ROOT/etc/adsbpi.toml --file testdata/modes1.bin 2>&1 | grep -q '260 messages'"
check "binary reads the home from the config" "$BIN --config $ROOT/etc/adsbpi.toml --file testdata/modes1.bin 2>&1 | grep -q 'home position 32.96180, -96.82920'"

echo "== re-run keeps the config"
echo '# my edit' >> "$ROOT/etc/adsbpi.toml"
run >/dev/null 2>&1
check "existing config preserved on upgrade" "grep -q '# my edit' $ROOT/etc/adsbpi.toml"

echo "== fresh install without a home"
ROOT2=$(mktemp -d)
DESTDIR="$ROOT2" ADSBPI_LOCAL_BIN="$BIN" sh ./install.sh >/dev/null 2>&1
check "config without home is valid" "$BIN --config $ROOT2/etc/adsbpi.toml --file testdata/modes1.bin 2>&1 | grep -q 'home position not set'"
rm -rf "$ROOT2"

echo "== bad inputs"
check "unknown flag rejected" "! sh ./install.sh --bogus >/dev/null 2>&1"
check "root required for a real install" "[ \"\$(id -u)\" = 0 ] || ! sh ./install.sh >/dev/null 2>&1"

echo "== uninstall"
run --uninstall >/dev/null 2>&1
check "binary, unit, udev and blacklist removed" "[ ! -e $ROOT/usr/local/bin/adsbpi ] && [ ! -e $ROOT/etc/systemd/system/adsbpi.service ] && [ ! -e $ROOT/etc/udev/rules.d/60-adsbpi-rtlsdr.rules ] && [ ! -e $ROOT/etc/modprobe.d/adsbpi-blacklist-rtl.conf ]"
check "config kept without --purge" "[ -e $ROOT/etc/adsbpi.toml ]"
run --uninstall --purge >/dev/null 2>&1
check "config removed with --purge" "[ ! -e $ROOT/etc/adsbpi.toml ]"

[ $fail = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit $fail
