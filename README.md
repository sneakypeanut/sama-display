# SAMA cooler temperature display

[![ci](https://github.com/sneakypeanut/sama-display/actions/workflows/ci.yml/badge.svg)](https://github.com/sneakypeanut/sama-display/actions/workflows/ci.yml)

Shows CPU and GPU temperatures on the 480×480 round screen of SAMA liquid
coolers on Linux. The screen's firmware reports itself as
`chs_5inch.dev1_rom1.89`; despite the name, the panel is 480×480.

It is a single Rust executable. It sends only display commands and CDC
DTR/RTS, and only reads temperatures: it has **no fan, pump, RGB, firmware or
speed controls**.

## Build and run

Requires Rust 1.85+ (2024 edition), Cargo and a C toolchain. `libusb` is built
from the vendored Rust dependency, so no system libusb development package is
needed. At runtime a Liberation Sans or DejaVu Sans font must be installed; the
renderer exits with an error if neither is found.

```sh
cargo build --release --locked
./install.sh --dry-run
# Stop any running instance before upgrading. The installer does not use sudo
# and does not enable or start any service.
./install.sh

"$HOME/.local/share/sama-display/bin/sama-display" --frames 2
# Other options: --once, --interval 3, --rotation 0|90|180|270, --state-dir PATH
```

The default rotation is **90° counterclockwise**. The clock, readings and
preview image update on every frame the screen acknowledges.

`--interval` is the minimum time from the start of one frame to the start of
the next, not a pause after each transfer. If sampling, rendering, transfer and
acknowledgement take longer, the next frame starts as soon as the previous one
finishes; frames never overlap and missed frames are not sent in a burst.
`--frames N` counts acknowledged frames in the current run. A run with
`--frames` or `--once` exits with an error after three failed cycles in a row;
continuous mode keeps reconnecting with a backoff of 1, 2, 4, 8, 16 and then
30 seconds.

## Device identity and permissions

The awake screen is found by **1d6b:0121 / serial 20080411**, never by bus
address. When the screen is asleep it appears as a USB bridge,
**1a86:ca21 / serial CT21INCH**. The app finds that bridge's tty through sysfs
and opens it to assert DTR/RTS to try to wake the screen; it sends no data to
it. A missing tty driver or unsupported ioctl is logged and not treated as a
successful wake. Wake after a cold boot has not been tested yet. Once the screen
is awake, the app detaches the kernel drivers from interfaces 0 and 1 as
needed, claims interface 1 and asserts CDC DTR/RTS on interface 0.

The installer generates `$HOME/.local/share/sama-display/70-sama-display.rules`.
The rule grants the installing user's UID access to the USB and tty nodes that
match these exact IDs and serials, with mode 0660. The installer does **not**
write to `/etc` or ask for root. Review the rule, then install it as an
administrator:

```sh
sudo install -m 0644 "$HOME/.local/share/sama-display/70-sama-display.rules" /etc/udev/rules.d/70-sama-display.rules
sudo udevadm control --reload-rules
# Stop the app, then replug the device or trigger udev for it.
```

The rule is specific to one user; regenerate it to use another account. No
world-writable rule or broad USB access is needed.

## User service

The installer copies the release binary to
`~/.local/share/sama-display/bin/sama-display` and the unit to
`~/.config/systemd/user/sama-display.service`. It does not run systemctl, so
enable the service yourself:

```sh
systemctl --user daemon-reload
systemctl --user enable --now sama-display.service
journalctl --user -u sama-display.service -f
systemctl --user stop sama-display.service
```

The unit is wanted by `default.target`, restarts on failure after five seconds
and has five seconds to stop. User services start at login; to start at boot
without logging in, an administrator must enable lingering
(`loginctl enable-linger USER`). The installer does not change this.

SIGTERM or SIGINT interrupts any wait within 200 ms. Reads poll every 200 ms or
less, each 16000-byte write times out after 500 ms, and `nvidia-smi` gets two
seconds. On exit, unplug or error it releases the USB interfaces and
tries to reattach the kernel drivers it detached. It sends no media, fan or
pump commands while cleaning up. A lock on the state directory prevents two
instances from sharing it, so do not run the service and a manual copy with the
same state directory.

## Protocol and state

HELLO waits up to eight seconds for the full supported ROM identity, and
STOP79/STOP96 wait up to eight seconds for `media_stop`. Queued identity replies
and stale acknowledgements are drained, with a time limit, before each frame.
Frames are full BGRA images, encoded in the screen's 250-byte blocks and written
in chunks of at most 16000 bytes. After PRE86, the comma preamble and the frame
transfer, the app sends one QUERY and waits up to twelve seconds for
**`full_png_sucess` or `full_png_success`** (the firmware misspells it), which
may arrive split across reads. A plain `success` is not accepted, and an empty
poll never resends the frame. After any error the app drops the session,
releases USB, backs off, finds the device again and repeats the handshake before
the next frame. The firmware does not number its replies, so draining and
sending one frame at a time reduce, but cannot rule out, a reply being matched
to the wrong frame.

The default state directory is
`${XDG_STATE_HOME:-$HOME/.local/state}/sama-display`; the installed unit sets
`~/.local/state/sama-display` explicitly. After each acknowledged frame the app
writes `status.json` and the rotated `latest-frame.png`. Each file is written to
a temporary file, synced and renamed into place, but the two are not updated
together. `status.json` holds the frame `sequence`, UTC timestamps for the start,
sample and acknowledgement, transfer-plus-acknowledgement latency, full cycle
latency, CPU and GPU temperatures, rotation, ROM identity and acknowledgement
text. Failed frames leave the last status in place, so check its timestamp to
see whether it is current. The sequence continues from the previous valid
status.

## Upgrading from the Python prototype

Earlier versions were a Python script installed under
`~/.local/share/sama-display` with a virtualenv. The Rust installer leaves those
files in place so you can roll back: restore the old unit's `ExecStart` and
restart the service. Once the Rust version works, delete the old venv, script
and requirements file; the Rust unit does not use them.

## Tests

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
./install.sh --dry-run
```

The tests use fake transports, clocks, sensors and temporary state directories,
and never touch USB. They check the 250-byte wire encoding against digests from
the original Python implementation, split and delayed acknowledgements,
timeouts, handshake order, one frame in flight at a time, reconnect timing,
release on cancellation, rendering and rotation, failed state writes, CLI
validation, subprocess time limits, bridge matching, chunked writes and the
installer. They do not cover real hardware, live sensors, hotplug or reboot.
