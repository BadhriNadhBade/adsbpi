#!/bin/sh
# adsbpi installer for Raspberry Pi (and other Linux aarch64 / x86_64 boxes with an RTL-SDR).
#
#   curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/install.sh | sudo sh
#
# Optional, set before `sh`:
#   ADSBPI_LAT / ADSBPI_LON   your antenna position, e.g.  ... | sudo ADSBPI_LAT=32.9618 ADSBPI_LON=-96.8292 sh
#   ADSBPI_VERSION            install a specific release tag instead of the latest (e.g. v0.1.0)
# Flags (use `sh -s -- <flag>` when piping):
#   --uninstall               remove the service, binary, udev rule and driver blacklist (keeps config)
#   --purge                   with --uninstall: also delete /etc/adsbpi.toml, saved state and the service user
#
# Safe to re-run: that upgrades the binary and keeps your config.

set -eu

REPO="${ADSBPI_REPO:-BadhriNadhBade/adsbpi}"
DESTDIR="${DESTDIR:-}"      # staging root, used by tests; skips every system-changing step
BIN_PATH=/usr/local/bin/adsbpi
CONF=/etc/adsbpi.toml
UNIT=/etc/systemd/system/adsbpi.service
UDEV=/etc/udev/rules.d/60-adsbpi-rtlsdr.rules
BLACKLIST=/etc/modprobe.d/adsbpi-blacklist-rtl.conf
STATE=/var/lib/adsbpi
SVC_USER=adsbpi

say()  { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
live() { [ -z "$DESTDIR" ]; }                       # are we changing the real system?
have() { command -v "$1" >/dev/null 2>&1; }
dest() { printf '%s%s' "$DESTDIR" "$1"; }

ACTION=install
PURGE=0
for a in "$@"; do
  case "$a" in
    --uninstall) ACTION=uninstall ;;
    --purge)     PURGE=1 ;;
    -h|--help)   echo "usage: install.sh [--uninstall [--purge]]   (env: ADSBPI_LAT ADSBPI_LON ADSBPI_VERSION)"; exit 0 ;;
    *) die "unknown option: $a" ;;
  esac
done

if live && [ "$(id -u)" -ne 0 ]; then
  die "needs root. Run it with sudo:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh"
fi

systemd_running() { live && have systemctl && [ -d /run/systemd/system ]; }

# ---------------------------------------------------------------- uninstall
if [ "$ACTION" = uninstall ]; then
  say "Uninstalling adsbpi"
  if systemd_running; then
    systemctl disable --now adsbpi 2>/dev/null || true
  fi
  rm -f "$(dest $UNIT)" "$(dest $BIN_PATH)" "$(dest $UDEV)" "$(dest $BLACKLIST)"
  if systemd_running; then systemctl daemon-reload || true; fi
  if live && have udevadm; then udevadm control --reload-rules 2>/dev/null || true; fi
  if [ "$PURGE" = 1 ]; then
    rm -f "$(dest $CONF)"
    rm -rf "$(dest $STATE)"
    if live && id "$SVC_USER" >/dev/null 2>&1; then userdel "$SVC_USER" 2>/dev/null || true; fi
    say "Removed config, saved state and the '$SVC_USER' user."
  else
    say "Kept $CONF and $STATE (use --uninstall --purge to delete them too)."
  fi
  say "Done. The kernel TV-tuner driver is no longer blacklisted; unplug and replug the dongle."
  exit 0
fi

# ---------------------------------------------------------------- platform
[ "$(uname -s)" = Linux ] || die "adsbpi installs on Linux only (this is $(uname -s))."
case "$(uname -m)" in
  aarch64|arm64) ARCH=aarch64 ;;
  x86_64|amd64)  ARCH=x86_64 ;;
  armv7l|armv6l) die "32-bit ARM is not supported. Install 64-bit Raspberry Pi OS (Pi 3/4/5 support it)." ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac

if have curl; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif have wget; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "need curl or wget (sudo apt install curl)"
fi
have sha256sum || die "need sha256sum (coreutils)"
have tar       || die "need tar"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------- get the binary
if [ -n "${ADSBPI_LOCAL_BIN:-}" ]; then
  say "Using local binary $ADSBPI_LOCAL_BIN (ADSBPI_LOCAL_BIN)"
  cp "$ADSBPI_LOCAL_BIN" "$TMP/adsbpi"
else
  ASSET="adsbpi-$ARCH-linux.tar.gz"
  if [ -n "${ADSBPI_VERSION:-}" ]; then
    BASE="https://github.com/$REPO/releases/download/$ADSBPI_VERSION"
  else
    BASE="https://github.com/$REPO/releases/latest/download"
  fi
  say "Downloading $ASSET"
  fetch "$BASE/$ASSET" "$TMP/$ASSET"            || die "download failed: $BASE/$ASSET"
  fetch "$BASE/$ASSET.sha256" "$TMP/$ASSET.sha256" || die "checksum download failed"
  say "Verifying checksum"
  ( cd "$TMP" && sha256sum -c "$ASSET.sha256" >/dev/null ) || die "checksum mismatch, refusing to install"
  tar -xzf "$TMP/$ASSET" -C "$TMP" adsbpi       || die "archive does not contain 'adsbpi'"
fi
chmod 0755 "$TMP/adsbpi"
if live; then
  "$TMP/adsbpi" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine"
fi

# ---------------------------------------------------------------- install files
if systemd_running; then systemctl stop adsbpi 2>/dev/null || true; fi

say "Installing $BIN_PATH"
mkdir -p "$(dest /usr/local/bin)"
cp "$TMP/adsbpi" "$(dest $BIN_PATH).new"
mv -f "$(dest $BIN_PATH).new" "$(dest $BIN_PATH)"

say "Freeing the dongle from the Linux TV-tuner driver"
mkdir -p "$(dest /etc/modprobe.d)"
cat > "$(dest $BLACKLIST)" <<'EOF'
# Installed by adsbpi: stop the kernel DVB driver from claiming RTL-SDR dongles.
blacklist dvb_usb_rtl28xxu
blacklist rtl2832
blacklist rtl2830
blacklist rtl2832_sdr
EOF
if live; then
  modprobe -r dvb_usb_rtl28xxu rtl2832_sdr rtl2832 rtl2830 2>/dev/null || true
fi

say "Allowing the service to open the USB dongle (udev rule)"
mkdir -p "$(dest /etc/udev/rules.d)"
cat > "$(dest $UDEV)" <<'EOF'
# Installed by adsbpi: Realtek RTL2832U dongles, accessible to members of 'plugdev'.
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0660", GROUP="plugdev", TAG+="uaccess"
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2832", MODE="0660", GROUP="plugdev", TAG+="uaccess"
EOF

if live; then
  getent group plugdev >/dev/null 2>&1 || groupadd --system plugdev
  if ! id "$SVC_USER" >/dev/null 2>&1; then
    say "Creating service user '$SVC_USER'"
    NOLOGIN=/usr/sbin/nologin; [ -x "$NOLOGIN" ] || NOLOGIN=/sbin/nologin; [ -x "$NOLOGIN" ] || NOLOGIN=/bin/false
    useradd --system --no-create-home --home-dir "$STATE" --shell "$NOLOGIN" --groups plugdev "$SVC_USER"
  fi
  if have udevadm; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=usb --action=add 2>/dev/null || true
  fi
fi

# Config: never overwrite an existing one (upgrades keep your settings).
if [ -e "$(dest $CONF)" ]; then
  say "Keeping existing $CONF"
  if [ -n "${ADSBPI_LAT:-}${ADSBPI_LON:-}" ]; then
    warn "ADSBPI_LAT/LON ignored because $CONF already exists; edit it or use 'Set home' on the map."
  fi
else
  say "Writing $CONF"
  mkdir -p "$(dest /etc)"
  {
    cat <<'EOF'
# adsbpi configuration. After editing:  sudo systemctl restart adsbpi
# Your antenna position can also be set from the web map ("Set home"); that choice is saved
# under /var/lib/adsbpi and takes priority over the [home] table below.

# RTL-SDR gain in dB (0 = automatic). 31 is a good start for 1090 MHz.
gain_db = 31.0
# bias_tee = false            # true powers an LNA through the antenna cable
# device_index = 0            # which dongle, if you have several

# Where to listen. The map and JSON are on the LAN; Beast/SBS are local-only by default.
web_bind   = "0.0.0.0:8080"
beast_bind = "127.0.0.1:30005"
sbs_bind   = "127.0.0.1:30003"

# ttl_secs = 60               # forget aircraft not heard for this long
# max_range_km = 500          # reject positions further than this from home
EOF
    if [ -n "${ADSBPI_LAT:-}" ] && [ -n "${ADSBPI_LON:-}" ]; then
      printf '\n[home]\nlat = %s\nlon = %s\n' "$ADSBPI_LAT" "$ADSBPI_LON"
    else
      printf '\n# [home]\n# lat = 32.9618\n# lon = -96.8292\n'
    fi
  } > "$(dest $CONF)"
  chmod 0644 "$(dest $CONF)"
fi

say "Installing systemd service"
mkdir -p "$(dest /etc/systemd/system)"
cat > "$(dest $UNIT)" <<EOF
[Unit]
Description=adsbpi ADS-B receiver, decoder and live map
Documentation=https://github.com/$REPO
After=network-online.target
Wants=network-online.target

[Service]
User=$SVC_USER
Group=$SVC_USER
SupplementaryGroups=plugdev
ExecStart=$BIN_PATH --config $CONF
Restart=on-failure
RestartSec=3
StateDirectory=adsbpi
Environment=RUST_LOG=info
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF

# ---------------------------------------------------------------- start
if ! live; then
  say "Staged into $DESTDIR (no system changes were made)."
  exit 0
fi

if systemd_running; then
  systemctl daemon-reload
  systemctl enable adsbpi >/dev/null 2>&1
  systemctl restart adsbpi
  sleep 3
  if systemctl is-active --quiet adsbpi; then
    say "adsbpi is running."
  else
    warn "the service did not stay up. Look at:  journalctl -u adsbpi -n 30 --no-pager"
  fi
else
  warn "systemd is not running here, so the service was not started. Run it by hand:"
  warn "  $BIN_PATH --config $CONF"
fi

IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
HOST="$(hostname 2>/dev/null || echo raspberrypi)"
echo
echo "  Live map:   http://${HOST}.local:8080/${IP:+   (or http://$IP:8080/)}"
echo "  Check dongle:  adsbpi probe"
echo "  Logs:          journalctl -u adsbpi -f"
echo "  Config:        $CONF   (restart with: sudo systemctl restart adsbpi)"
if [ -z "${ADSBPI_LAT:-}" ] && [ ! -s "$STATE/home.json" ]; then
  echo
  echo "  Set your antenna position: open the map and press 'Set home', then click your location."
fi
echo "  Uninstall:     curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh -s -- --uninstall"
