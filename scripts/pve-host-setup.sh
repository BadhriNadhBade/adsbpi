#!/bin/sh
# Run this ON THE PROXMOX NODE (not inside the container) to give an LXC container access to an
# RTL-SDR dongle:
#
#   curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/scripts/pve-host-setup.sh | sh -s -- <CTID>
#
# What it does (all reversible with --undo):
#   1. blacklists the Linux TV-tuner drivers on the host, so they do not claim the dongle
#   2. adds a udev rule making only the Realtek RTL2832U dongles (0bda:2838 / 0bda:2832) readable
#      and writable by every local user. Needed because an unprivileged container sees host users
#      as "nobody", so group-based access cannot work
#   3. adds a marked block to /etc/pve/lxc/<CTID>.conf that allows USB devices (char major 189)
#      and bind-mounts /dev/bus/usb into the containe
# Then restart the container:  pct reboot <CTID>
#
# Test hook: ROOT=/some/dir stages every file under that directory and skips the live steps;
# PVE_LXC_DIR points at the container config directory (default /etc/pve/lxc).

set -eu

ROOT="${ROOT:-}"
LXC_DIR="${PVE_LXC_DIR:-$ROOT/etc/pve/lxc}"
BLACKLIST="$ROOT/etc/modprobe.d/adsbpi-blacklist-rtl.conf"
UDEV="$ROOT/etc/udev/rules.d/60-adsbpi-rtlsdr-host.rules"
BEGIN="# adsbpi-usb-begin"
END="# adsbpi-usb-end"

die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
say()  { printf '==> %s\n' "$*"; }
live() { [ -z "$ROOT" ]; }

UNDO=0
CTID=""
for a in "$@"; do
  case "$a" in
    --undo) UNDO=1 ;;
    -h|--help) echo "usage: pve-host-setup.sh <container-id> [--undo]   (run on the Proxmox node)"; exit 0 ;;
    -*) die "unknown option: $a" ;;
    *)  [ -z "$CTID" ] || die "only one container id allowed"; CTID=$a ;;
  esac
done
[ -n "$CTID" ] || die "usage: pve-host-setup.sh <container-id> [--undo]"
case "$CTID" in *[!0-9]*) die "container id must be a number, got '$CTID'" ;; esac

if live; then
  [ "$(id -u)" -eq 0 ] || die "run as root on the Proxmox node"
  command -v pct >/dev/null 2>&1 || die "'pct' not found: this script runs on the Proxmox host, not inside a container"
fi
CONF="$LXC_DIR/$CTID.conf"
[ -f "$CONF" ] || die "no such container: $CONF"

# Remove an existing adsbpi block from the container config (so re-running never duplicates it).
strip_block() {
  tmp="$CONF.adsbpi.$$"
  awk -v b="$BEGIN" -v e="$END" '$0==b{skip=1;next} $0==e{skip=0;next} !skip{print}' "$CONF" > "$tmp"
  cat "$tmp" > "$CONF"      # keep the file's identity/permissions (/etc/pve is a special filesystem)
  rm -f "$tmp"
}

if [ "$UNDO" = 1 ]; then
  say "Removing the USB passthrough block from $CONF"
  strip_block
  rm -f "$BLACKLIST" "$UDEV"
  if live && command -v udevadm >/dev/null 2>&1; then udevadm control --reload-rules || true; fi
  say "Done. Restart the container (pct reboot $CTID); unplug and replug the dongle."
  exit 0
fi

say "Blacklisting the TV-tuner drivers on the host"
mkdir -p "$(dirname "$BLACKLIST")"
cat > "$BLACKLIST" <<'EOF'
# Installed by adsbpi (pve-host-setup.sh): keep the Linux DVB driver away from RTL-SDR dongles.
blacklist dvb_usb_rtl28xxu
blacklist rtl2832
blacklist rtl2830
blacklist rtl2832_sd
EOF
if live; then
  modprobe -r dvb_usb_rtl28xxu rtl2832_sdr rtl2832 rtl2830 2>/dev/null || true
  if command -v update-initramfs >/dev/null 2>&1; then update-initramfs -u >/dev/null 2>&1 || true; fi
fi

say "Making RTL2832U dongles accessible to containers (udev)"
mkdir -p "$(dirname "$UDEV")"
cat > "$UDEV" <<'EOF'
# Installed by adsbpi (pve-host-setup.sh): Realtek RTL2832U dongles usable from LXC containers.
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0666"
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2832", MODE="0666"
EOF
if live && command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=usb --action=add || true
fi

say "Adding USB passthrough to $CONF"
strip_block
# The block must go before the first [section] (snapshots), or it would belong to that snapshot.
tmp="$CONF.adsbpi.$$"
awk -v b="$BEGIN" -v e="$END" '
  function block() {
    print b
    print "lxc.cgroup2.devices.allow: c 189:* rwm"
    print "lxc.mount.entry: /dev/bus/usb dev/bus/usb none bind,optional,create=dir"
    print e
    done = 1
  }
  /^\[/ && !done { block() }
  { print }
  END { if (!done) block() }
' "$CONF" > "$tmp"
cat "$tmp" > "$CONF"
rm -f "$tmp"

if ! live; then say "Staged under $ROOT (no system changes were made)."; exit 0; fi
echo
echo "Done. Next:"
echo "  1. pct reboot $CTID            (restart the container so it sees /dev/bus/usb)"
echo "  2. inside the container: curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/install.sh | sh"
echo "  3. inside the container: adsbpi probe"
echo "Undo with: sh pve-host-setup.sh $CTID --undo"
