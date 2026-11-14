#!/bin/sh
# adsbpi installer for Proxmox VE: run it on the Proxmox HOST (not inside a container).
#
#   curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/proxmox.sh | sh
#
# It creates a small Debian LXC container, passes the RTL-SDR USB dongle through to it, and runs
# install.sh inside. A container shares the host kernel, so the host (not the container) has to
# keep the Linux TV-tuner driver off the dongle and make the USB node accessible; this does both.
#
# Optional, set before `sh`:
#   ADSBPI_LAT / ADSBPI_LON   antenna position
#   ADSBPI_CTID               use this existing container instead of creating one (it gets restarted)
#   ADSBPI_HOSTNAME           new container's hostname            (default adsbpi)
#   ADSBPI_STORAGE            storage for the container disk      (default: first that allows rootdir)
#   ADSBPI_BRIDGE             network bridge                      (default vmbr0)
#   ADSBPI_VERSION            install a specific release tag
#
# Safe to re-run with ADSBPI_CTID set: that upgrades adsbpi in that container.

set -eu

REPO="${ADSBPI_REPO:-BadhriNadhBade/adsbpi}"
RAW="https://raw.githubusercontent.com/$REPO/main"
HOST_BLACKLIST=/etc/modprobe.d/adsbpi-blacklist-rtl.conf
HOST_UDEV=/etc/udev/rules.d/60-adsbpi-rtlsdr-host.rules

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ]          || die "run as root on the Proxmox host."
command -v pct >/dev/null 2>&1 || die "'pct' not found. This script is for the Proxmox VE host, not inside a container (inside one, use install.sh)."
command -v curl >/dev/null 2>&1 || die "need curl on the host."

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------- host: dongle access
say "Freeing the dongle from the Linux TV-tuner driver (on the host)"
cat > "$HOST_BLACKLIST" <<'EOF'
# Installed by adsbpi: stop the kernel DVB driver from claiming RTL-SDR dongles.
blacklist dvb_usb_rtl28xxu
blacklist rtl2832
blacklist rtl2830
blacklist rtl2832_sdr
EOF
modprobe -r dvb_usb_rtl28xxu rtl2832_sdr rtl2832 rtl2830 2>/dev/null || true

say "Making the dongle's USB node openable from the container (host udev rule)"
cat > "$HOST_UDEV" <<'EOF'
# Installed by adsbpi: Realtek RTL2832U dongles, openable inside an unprivileged LXC container.
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0666"
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2832", MODE="0666"
EOF
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger --subsystem-match=usb --action=add 2>/dev/null || true

if command -v lsusb >/dev/null 2>&1 && ! lsusb | grep -qi '0bda:28[23][28]'; then
  printf 'warning: no RTL-SDR (0bda:2838) is plugged into this host right now. Plug it in; the container will pick it up.\n' >&2
fi

# ---------------------------------------------------------------- container: create or reuse
if [ -n "${ADSBPI_CTID:-}" ]; then
  CTID="$ADSBPI_CTID"
  pct config "$CTID" >/dev/null 2>&1 || die "container $CTID does not exist."
  say "Using existing container $CTID"
  NEW=0
else
  NEW=1
  CTID="$(pvesh get /cluster/nextid 2>/dev/null)" || die "could not get a free container id."
  STORAGE="${ADSBPI_STORAGE:-$(pvesm status --content rootdir 2>/dev/null | awk 'NR>1 {print $1; exit}')}"
  [ -n "$STORAGE" ] || die "no storage allows container disks. Set ADSBPI_STORAGE."

  say "Finding a Debian template"
  pveam update >/dev/null 2>&1 || true
  HOST_ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"
  # 'pveam available' lists every architecture, so match the host's or /sbin/init won't exec.
  TEMPLATE="$(pveam available --section system 2>/dev/null | awk -v a="_${HOST_ARCH}.tar" '$2 ~ /^debian-1[23]-standard/ && index($2, a) {print $2}' | sort -V | tail -n 1)"
  [ -n "$TEMPLATE" ] || die "no Debian template for $HOST_ARCH available from 'pveam available'."
  TSTORE="$(pvesm status --content vztmpl 2>/dev/null | awk 'NR>1 {print $1; exit}')"
  [ -n "$TSTORE" ] || die "no storage allows container templates."
  if ! pveam list "$TSTORE" 2>/dev/null | grep -q "$TEMPLATE"; then
    say "Downloading $TEMPLATE"
    pveam download "$TSTORE" "$TEMPLATE" >/dev/null
  fi

  say "Creating container $CTID (${ADSBPI_HOSTNAME:-adsbpi}) on $STORAGE"
  pct create "$CTID" "$TSTORE:vztmpl/$TEMPLATE" \
    --hostname "${ADSBPI_HOSTNAME:-adsbpi}" \
    --cores 1 --memory 256 --swap 0 \
    --rootfs "$STORAGE:2" \
    --net0 "name=eth0,bridge=${ADSBPI_BRIDGE:-vmbr0},ip=dhcp" \
    --unprivileged 1 --features nesting=1 --onboot 1 >/dev/null
fi

# ---------------------------------------------------------------- container: USB passthrough
CONF="/etc/pve/lxc/$CTID.conf"
add_conf() { grep -qxF "$1" "$CONF" || echo "$1" >> "$CONF"; }
say "Passing /dev/bus/usb through to container $CTID"
add_conf 'lxc.cgroup2.devices.allow: c 189:* rwm'
add_conf 'lxc.mount.entry: /dev/bus/usb dev/bus/usb none bind,optional,create=dir'

# (re)start so the new config takes effect
pct status "$CTID" | grep -q running && pct stop "$CTID"
pct start "$CTID"

say "Waiting for the container's network"
i=0
until pct exec "$CTID" -- getent hosts github.com >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -le 30 ] || die "container $CTID has no network after 60 s (check bridge/DHCP)."
  sleep 2
done

# ---------------------------------------------------------------- install inside
say "Installing adsbpi inside container $CTID"
curl -fsSL --retry 3 -o "$TMP/install.sh" "$RAW/install.sh" || die "could not download install.sh"
pct exec "$CTID" -- sh -c 'command -v curl >/dev/null 2>&1 || { apt-get update -qq && apt-get install -y -qq curl ca-certificates; }' >/dev/null
pct push "$CTID" "$TMP/install.sh" /root/adsbpi-install.sh
pct exec "$CTID" -- env \
  "ADSBPI_LAT=${ADSBPI_LAT:-}" "ADSBPI_LON=${ADSBPI_LON:-}" "ADSBPI_VERSION=${ADSBPI_VERSION:-}" "ADSBPI_REPO=$REPO" \
  sh /root/adsbpi-install.sh

IP="$(pct exec "$CTID" -- hostname -I 2>/dev/null | awk '{print $1}')"
echo
echo "  Container:  $CTID"
echo "  Live map:   http://${IP:-<container-ip>}:8080/"
echo "  Check dongle:  pct exec $CTID -- adsbpi probe"
echo "  Logs:          pct exec $CTID -- journalctl -u adsbpi -f"
[ "$NEW" = 1 ] && echo "  Remove:        pct stop $CTID && pct destroy $CTID"
exit 0
