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

echo "== container mode (LXC / Docker)"
ROOT3=$(mktemp -d)
DESTDIR="$ROOT3" ADSBPI_CONTAINER=1 ADSBPI_LOCAL_BIN="$BIN" sh ./install.sh >"$ROOT3/out" 2>&1
check "container: driver blacklist skipped (it belongs on the host)" "[ ! -e $ROOT3/etc/modprobe.d/adsbpi-blacklist-rtl.conf ] && grep -q 'Container detected' $ROOT3/out"
check "container: service, config and udev rule still installed" "[ -e $ROOT3/etc/systemd/system/adsbpi.service ] && [ -e $ROOT3/etc/adsbpi.toml ] && [ -e $ROOT3/etc/udev/rules.d/60-adsbpi-rtlsdr.rules ]"
rm -rf "$ROOT3"

echo "== Proxmox host helper (staged)"
PVE=$(mktemp -d); mkdir -p "$PVE/etc/pve/lxc"
CT="$PVE/etc/pve/lxc/105.conf"
printf 'arch: amd64\nhostname: adsb\nunprivileged: 1\n\n[before-upgrade]\narch: amd64\nhostname: adsb\n' > "$CT"
pve() { ROOT="$PVE" sh ./scripts/pve-host-setup.sh "$@"; }
pve 105 >/dev/null 2>&1
check "pve: passthrough lines added" "grep -q '^lxc.cgroup2.devices.allow: c 189:\* rwm' $CT && grep -q '^lxc.mount.entry: /dev/bus/usb dev/bus/usb none bind,optional,create=dir' $CT"
check "pve: block sits before the first snapshot section" "[ \$(grep -n 'adsbpi-usb-begin' $CT | cut -d: -f1) -lt \$(grep -n '^\[before-upgrade\]' $CT | cut -d: -f1) ]"
check "pve: original settings untouched" "grep -q '^unprivileged: 1' $CT && grep -q '^hostname: adsb' $CT"
check "pve: host blacklist and udev rule written" "grep -q 'blacklist dvb_usb_rtl28xxu' $PVE/etc/modprobe.d/adsbpi-blacklist-rtl.conf && grep -q 'idProduct}==\"2838\".*MODE=\"0666\"' $PVE/etc/udev/rules.d/60-adsbpi-rtlsdr-host.rules"
pve 105 >/dev/null 2>&1
check "pve: re-run is idempotent (one block)" "[ \$(grep -c 'adsbpi-usb-begin' $CT) = 1 ]"
pve 105 --undo >/dev/null 2>&1
check "pve: undo removes block, blacklist and udev rule" "! grep -q 'adsbpi' $CT && [ ! -e $PVE/etc/modprobe.d/adsbpi-blacklist-rtl.conf ] && [ ! -e $PVE/etc/udev/rules.d/60-adsbpi-rtlsdr-host.rules ] && grep -q '^\[before-upgrade\]' $CT"
check "pve: unknown container rejected" "! pve 999 >/dev/null 2>&1"
check "pve: non-numeric id rejected" "! pve abc >/dev/null 2>&1"
check "pve: refuses to run inside a container/non-Proxmox box" "! sh ./scripts/pve-host-setup.sh 105 >/dev/null 2>&1"
rm -rf "$PVE"

[ $fail = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit $fail
