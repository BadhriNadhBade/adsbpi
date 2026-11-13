#!/usr/bin/env bash
# Real-container test: installs adsbpi inside Debian LXC containers (privileged AND unprivileged,
# the way Proxmox runs them) and checks the service, web map, Beast/SBS ports and uninstall.
#
# Needs root, the `lxc` package and internet (it downloads a Debian rootfs once per container).
# No dongle is needed; the radio itself cannot be tested this way (see the README, "Proxmox / LXC").
#
# Usage: sudo scripts/test-lxc.sh [path-to-x86_64-binary]
# Env:   KEEP=1  leave the containers behind for inspection
set -u
cd "$(dirname "$0")/.."
BIN=${1:-./target/x86_64-unknown-linux-musl/release/adsbpi}
[ "$(id -u)" = 0 ] || { echo "run as root (sudo)"; exit 2; }
command -v lxc-create >/dev/null || { echo "install lxc first (apt install lxc)"; exit 2; }
[ -x "$BIN" ] || { echo "binary not found: $BIN"; exit 2; }

fail=0
check() { # check "name" command...   (runs the command, PASS/FAIL by exit status)
  local name=$1; shift
  if "$@" >/dev/null 2>&1; then echo "PASS  $name"; else echo "FAIL  $name"; fail=1; fi
}
push() { local d=/var/lib/lxc/$1/rootfs$3; mkdir -p "$(dirname "$d")" && cp "$2" "$d" && chmod 755 "$d"; }   # push CT local-file path-in-CT (host-side copy into the rootfs)
in_ct() { local ct=$1; shift; lxc-attach -n "$ct" -- "$@"; }

# Runs inside the container as root. Prints "ok:<name>" / "bad:<name>" lines.
INNER=$(mktemp)
cat > "$INNER" <<'EOF'
#!/bin/bash
ok()  { echo "ok:$1"; }
bad() { echo "bad:$1"; }
t()   { if eval "$2" >/dev/null 2>&1; then ok "$1"; else bad "$1"; fi; }

# wait for systemd, then make sure curl exists (minimal templates may lack it)
systemctl is-system-running --wait >/dev/null 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do getent hosts deb.debian.org >/dev/null 2>&1 && break; sleep 2; done
command -v curl >/dev/null || { apt-get update -qq && apt-get install -y -qq curl ca-certificates; } >/dev/null 2>&1

export ADSBPI_LOCAL_BIN=/var/tmp/adsbpi-bin ADSBPI_LAT=32.9618 ADSBPI_LON=-96.8292
sh /var/tmp/install.sh > /var/tmp/install.out 2>&1; t "installer exits 0" "[ $? = 0 ]"
sleep 4
t "service is active"            "systemctl is-active --quiet adsbpi"
t "service enabled at boot"      "systemctl is-enabled --quiet adsbpi"
t "web map served on :8080"      "curl -fsS http://127.0.0.1:8080/ | grep -qi leaflet"
t "/api/status answers"          "curl -fsS http://127.0.0.1:8080/api/status | grep -q uptime"
t "/data/aircraft.json answers"  "curl -fsS http://127.0.0.1:8080/data/aircraft.json | grep -q aircraft"
t "Beast port 30005 listening"   "exec 3<>/dev/tcp/127.0.0.1/30005"
t "SBS port 30003 listening"     "exec 3<>/dev/tcp/127.0.0.1/30003"
t "home from installer applied"  "journalctl -u adsbpi --no-pager | grep -q 'home position 32.96180'"
t "no-dongle is handled cleanly (service keeps running)" "systemctl is-active --quiet adsbpi"
t "probe reports a missing dongle without crashing" "adsbpi probe 2>&1 | grep -qi 'no rtl-sdr\|0 device\|could not\|not found'"
t "decodes the recording" "adsbpi --file /var/tmp/modes1.bin --web-bind 127.0.0.1:18080 --beast-bind 127.0.0.1:30105 --sbs-bind 127.0.0.1:30103 2>&1 | grep -q '260 messages'"
t "installer recognised the container and skipped the host-only driver blacklist" "grep -q 'Container detected' /var/tmp/install.out && [ ! -e /etc/modprobe.d/adsbpi-blacklist-rtl.conf ]"
if [ ! -d /dev/bus/usb ]; then
  t "installer warns that USB is not passed through" "grep -q 'not passed through' /var/tmp/install.out"
  t "service log tells the user to pass USB through" "journalctl -u adsbpi --no-pager | grep -q 'pass the USB device through'"
fi
t "service user exists"          "id adsbpi"
t "service survives restart"     "systemctl restart adsbpi && sleep 2 && systemctl is-active --quiet adsbpi"
echo '# my edit' >> /etc/adsbpi.toml
sh /var/tmp/install.sh > /var/tmp/upgrade.out 2>&1; sleep 3
t "upgrade keeps config"         "grep -q '# my edit' /etc/adsbpi.toml"
t "service active after upgrade" "systemctl is-active --quiet adsbpi"
if [ "$IN_CT_EXPECT_FAKE_USB" = 1 ]; then
  t "service user can open a 0666 USB node through the bind mount" "su -s /bin/sh adsbpi -c 'head -c1 /dev/bus/usb/001/002'"
fi
sh /var/tmp/install.sh --uninstall --purge > /var/tmp/uninstall.out 2>&1
t "uninstall stops and removes the service" "! systemctl is-active --quiet adsbpi && [ ! -e /etc/systemd/system/adsbpi.service ] && [ ! -e /usr/local/bin/adsbpi ]"
t "purge removes config"         "[ ! -e /etc/adsbpi.toml ]"
exit 0
EOF

run_mode() {
  local mode=$1 ct=adsbpi-test-$1 cfg
  echo; echo "===== $mode container ($ct)"
  lxc-stop -k -n "$ct" >/dev/null 2>&1; lxc-destroy -n "$ct" >/dev/null 2>&1
  cfg=$(mktemp)
  {
    echo "lxc.include = /usr/share/lxc/config/common.conf"
    if [ "$mode" = unprivileged ]; then
      echo "lxc.include = /usr/share/lxc/config/userns.conf"
      echo "lxc.idmap = u 0 100000 65536"
      echo "lxc.idmap = g 0 100000 65536"
    fi
    echo "lxc.net.0.type = veth"; echo "lxc.net.0.link = lxcbr0"; echo "lxc.net.0.flags = up"
  } > "$cfg"
  # root-owned unprivileged containers (what Proxmox uses) need a subuid/subgid range for root
  grep -q "^root:100000:65536" /etc/subuid 2>/dev/null || echo "root:100000:65536" >> /etc/subuid
  grep -q "^root:100000:65536" /etc/subgid 2>/dev/null || echo "root:100000:65536" >> /etc/subgid
  lxc-create -n "$ct" -f "$cfg" -t download -- -d debian -r bookworm -a "$(dpkg --print-architecture)" >/dev/null 2>&1 \
    || { echo "FAIL  could not create the container (internet? lxcbr0?)"; fail=1; return; }

  # fake USB tree to prove a 0666 device node is usable by the unprivileged service use
  local fake=/var/tmp/adsbpi-fakeusb expect_fake=0
  if [ "$mode" = unprivileged ]; then
    rm -rf "$fake"; mkdir -p "$fake/001"; chmod 755 "$fake" "$fake/001"
    echo x > "$fake/001/002"; chmod 666 "$fake/001/002"; chmod 755 /var/tmp
    echo "lxc.mount.entry = $fake dev/bus/usb none bind,create=dir 0 0" >> /var/lib/lxc/"$ct"/config
    expect_fake=1
  fi

  lxc-start -n "$ct" || { echo "FAIL  container did not start"; fail=1; return; }
  for _ in $(seq 30); do lxc-attach -n "$ct" -- true >/dev/null 2>&1 && break; sleep 1; done
  uidmap=$(lxc-attach -n "$ct" -- cat /proc/self/uid_map | tr -s ' ' | tr '\n' ';')
  if [ "$mode" = unprivileged ]; then
    check "$mode: root in the container is a mapped (non-host) uid" test "${uidmap#*100000}" != "$uidmap"
  else
    check "$mode: root in the container is host root" test "${uidmap#*100000}" = "$uidmap"
  fi

  push "$ct" "$BIN" /var/tmp/adsbpi-bin
  push "$ct" install.sh /var/tmp/install.sh
  push "$ct" testdata/modes1.bin /var/tmp/modes1.bin
  push "$ct" "$INNER" /var/tmp/inner.sh
  out=$(lxc-attach -n "$ct" -v IN_CT_EXPECT_FAKE_USB=$expect_fake -- bash /var/tmp/inner.sh 2>&1)
  echo "$out" | grep -E '^(ok|bad):' | while IFS=: read -r s name; do
    [ "$s" = ok ] && echo "PASS  $mode: $name" || echo "FAIL  $mode: $name"
  done
  echo "$out" | grep -q '^bad:' && fail=1
  if echo "$out" | grep -q '^bad:'; then
    echo "--- installer output"; lxc-attach -n "$ct" -- sh -c 'cat /var/tmp/install.out 2>/dev/null; journalctl -u adsbpi -n 15 --no-pager 2>/dev/null'
  fi
  [ "${KEEP:-0}" = 1 ] || { lxc-stop -k -n "$ct" >/dev/null 2>&1; lxc-destroy -n "$ct" >/dev/null 2>&1; }
  rm -f "$cfg"
}

run_mode privileged
run_mode unprivileged
rm -rf /var/tmp/adsbpi-fakeusb "$INNER"

echo
[ $fail = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit $fail
