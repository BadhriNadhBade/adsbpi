# adsbpi

One static binary that turns an RTL-SDR dongle into an ADS-B aircraft tracker: it reads the radio,
decodes Mode S / ADS-B, serves a live map in your browser, and re-publishes the feed in Beast and
SBS formats for other tools (Virtual Radar Server, tar1090, feeders, ...).

Written in Rust with no C dependencies (the USB and RTL2832U/R820T driver are pure Rust), so the
Raspberry Pi build is a single ~2 MB file with nothing else to install.

## Install on a Raspberry Pi (64-bit OS)

```sh
curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/install.sh | sudo sh
```

Add your antenna position in the same command (recommended):

```sh
curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/install.sh | sudo ADSBPI_LAT=32.9618 ADSBPI_LON=-96.8292 sh
```

Then open `http://<your-pi>.local:8080`. The installer:

1. downloads the right binary from the latest GitHub release and verifies its SHA-256,
2. stops the Linux TV-tuner driver from grabbing the dongle (modprobe blacklist, unloaded immediately)
   and adds a udev rule so the service can open it without root,
3. creates an unprivileged `adsbpi` user and `/etc/adsbpi.toml` (an existing config is never overwritten),
4. installs and starts the `adsbpi` systemd service (starts on boot, restarts on failure).

Re-running it upgrades in place. To remove everything: `... | sudo sh -s -- --uninstall`
(add `--purge` to also delete the config and saved state).

## Install in a Proxmox LXC container (or any x86_64 Linux server)

The same installer works on any 64-bit Intel/AMD or ARM Linux box with systemd (Debian, Ubuntu, ...).
A plain server or VM with the dongle plugged in needs nothing extra: use the command above.

An LXC container shares the host's kernel, so two things must be done **on the Proxmox node**, once:
stop the host's TV-tuner driver from grabbing the dongle, and pass the USB bus into the container.
One command does both (replace `105` with your container ID):

```sh
# on the Proxmox node, as root
curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/scripts/pve-host-setup.sh | sh -s -- 105
pct reboot 105
```

Then, inside the container (Debian/Ubuntu template, `curl` installed):

```sh
curl -fsSL https://raw.githubusercontent.com/BadhriNadhBade/adsbpi/main/install.sh | sh
adsbpi probe          # should list the dongle
```

The installer notices it is in a container, skips the host-only driver blacklist, and tells you if
`/dev/bus/usb` has not been passed through yet. The map is then at `http://<container-ip>:8080`.

What `pve-host-setup.sh` changes (undo with `... | sh -s -- 105 --undo`):

| Where | Change |
|---|---|
| `/etc/modprobe.d/adsbpi-blacklist-rtl.conf` | blacklists `dvb_usb_rtl28xxu`, `rtl2832`, `rtl2830`, `rtl2832_sdr` on the host |
| `/etc/udev/rules.d/60-adsbpi-rtlsdr-host.rules` | makes only `0bda:2838` / `0bda:2832` dongles `MODE=0666` (an unprivileged container sees host users as "nobody", so group permissions cannot work) |
| `/etc/pve/lxc/105.conf` | a marked block: `lxc.cgroup2.devices.allow: c 189:* rwm` and a bind mount of `/dev/bus/usb` |

Works for unprivileged (the Proxmox default) and privileged containers. If the service fails with
`status=226/NAMESPACE`, enable *Options, Features, Nesting* on the container. Replugging the dongle
is safe: the whole `/dev/bus/usb` directory is mounted, so the new device node appears by itself.
Other containers on the same node can see the dongle too; only one program can use it at a time.

## Using it

| What | Where |
|---|---|
| Live map | `http://<pi>:8080/` (OpenStreetMap tiles are fetched by your browser, so the *browser* needs internet) |
| Set antenna position | map header, **Set home**, click the map. Saved across restarts |
| Aircraft JSON (dump1090/tar1090 style) | `http://<pi>:8080/data/aircraft.json` |
| Status | `http://<pi>:8080/api/status`, `adsbpi probe`, `journalctl -u adsbpi -f` |
| Beast binary feed | `127.0.0.1:30005` |
| SBS / BaseStation feed | `127.0.0.1:30003` |

Beast and SBS listen on localhost only by default. To let another machine (e.g. a PC running
Virtual Radar Server: Receiver format **Beast**, port **30005**) connect, set
`beast_bind = "0.0.0.0:30005"` in `/etc/adsbpi.toml` and `sudo systemctl restart adsbpi`.

### Config (`/etc/adsbpi.toml`)

```toml
gain_db = 31.0                 # 0 = automatic
web_bind = "0.0.0.0:8080"
beast_bind = "127.0.0.1:30005"
sbs_bind = "127.0.0.1:30003"
# ttl_secs = 60                # drop aircraft not heard for this long
# max_range_km = 500           # reject positions further than this from home
# bias_tee = false
# device_index = 0

[home]
lat = 32.9618
lon = -96.8292
```

Unknown keys are rejected on purpose, so typos are reported instead of silently ignored.

### Why a home position matters

Aircraft broadcast positions in a compact form that needs either two messages or a reference
point to resolve. `adsbpi` uses your home position only as a sanity check (positions further than
`max_range_km` are discarded). If home is unset the check is skipped, nothing is rejected.

## Troubleshooting

* `adsbpi probe` lists dongles and tries to open one.
* "No RTL-SDR found": check `lsusb` shows `0bda:2838`; replug the dongle after installing.
* "could not open it": permissions or another program (e.g. dump1090, rtl_test) is using it.
* In a container, "/sys/bus/usb/devices not found" or "/dev/bus/usb does not exist": the USB bus is
  not passed through. Run `pve-host-setup.sh` on the Proxmox node (see above) and restart the container.
* Few aircraft: antenna placement dominates everything. Try gain 25 to 40 and a window or outdoor spot.
* Pi 5 note: use a good USB power supply; a dongle on a weak supply causes dropped samples
  (the log warns about dropped USB chunks).

## Build and test

```sh
cargo test                                  # unit tests
cargo build --release                       # host binary
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl   # the Pi binary, static

bash scripts/e2e.sh target/release/adsbpi   # replays a real recording through web, Beast and SBS
sh scripts/test-install.sh target/release/adsbpi   # installer in staging mode (touches nothing)
bash scripts/bench.sh target/release/adsbpi  # decode speed vs real time
sudo bash scripts/test-lxc.sh target/x86_64-unknown-linux-musl/release/adsbpi   # real Debian LXC containers
```

`scripts/test-lxc.sh` needs root, the `lxc` package and internet. It creates a privileged and an
unprivileged Debian 12 container, installs adsbpi in each with the real installer and checks the
service, web map, Beast/SBS ports, upgrade and uninstall, then deletes the containers (`KEEP=1` keeps
them). It adds `root:100000:65536` to `/etc/subuid` and `/etc/subgid` if missing. It cannot test the
dongle itself, since a USB radio cannot be simulated.

`adsbpi --file recording.bin` decodes a 2 MS/s unsigned 8-bit I/Q file (`rtl_sdr -f 1090000000 -s 2000000`)
and prints an aircraft table; add `--serve` to keep the map up afterwards.

### Releasing

`git tag v0.1.0 && git push origin v0.1.0` runs `.github/workflows/release.yml`, which tests, builds
static x86_64 and aarch64 binaries, and publishes `adsbpi-<arch>-linux.tar.gz` plus `.sha256` files
that the installer downloads.

## Design

```
RTL-SDR -> sdr (USB, 2 MS/s I/Q) -> demod (preamble + bits) -> modes (CRC, 1-bit repair, ICAO check)
        -> tracker (aircraft table, CPR positions, expiry) -> web / Beast / SBS / JSON
```

Limits worth knowing: 2 MS/s demodulation (like the original dump1090) rather than 2.4 MS/s;
single-bit error repair only; airborne positions only (no surface/ground movements);
no MLAT or Mode A/C.

MIT licensed. See `NOTICE` for third-party material.
