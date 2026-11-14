# SAMA cooler temperature display

[![ci](https://github.com/sneakypeanut/sama-display/actions/workflows/ci.yml/badge.svg)](https://github.com/sneakypeanut/sama-display/actions/workflows/ci.yml)

Linux user application for the observed **480×480 round** screen reporting
`chs_5inch.dev1_rom1.89` (the ROM name is not the panel dimensions). It is a
single Rust executable: no Python runtime is required. Only known display
commands and CDC DTR/RTS are sent. CPU/GPU readings are read-only; there are
**no fan, pump, RGB, firmware, or speed controls**.

## Build and run

Rust 1.85+ (2024 edition), Cargo and a C toolchain are required. `libusb` is
built from the vendored Rust dependency, so no system libusb development
package is needed. A system Liberation Sans or DejaVu Sans font is required at
runtime; the renderer fails clearly when neither exists.

```sh
cargo build --release --locked
./install.sh --dry-run
# Stop any prior instance before upgrading. Does not sudo, enable or start units.
./install.sh

"$HOME/.local/share/sama-display/bin/sama-display" --frames 2
# Alternative: --once, --interval 3, --rotation 0|90|180|270, --state-dir PATH
```

Default rotation is **90° counterclockwise**. The dark circular CPU/GPU layout
matches the tested prototype. The clock, readings and preview refresh on every
acknowledged frame.

`--interval` is a minimum **start-to-start** target, not a sleep after transfer.
If sampling, rendering, transfer and ACK take longer, the next frame starts
after completion; there are no overlapping frames or catch-up bursts. A nominal
three seconds is not a measured hardware guarantee. `--frames N` counts
acknowledged frames in this invocation, not the persisted sequence. Finite runs
exit nonzero after three consecutive failed cycles; continuous mode reconnects
indefinitely with 1, 2, 4, 8, 16, then 30-second backoff.

## Device identity and permissions

Discovery uses **1d6b:0121 / serial 20080411**, never a bus address. Sleeping
bridge **1a86:ca21 / serial CT21INCH** is located through tty sysfs ancestry;
opening its matched tty and asserting DTR/RTS is a best-effort wake (no serial
payload). Wake/re-enumeration still requires live verification on cold boot.
A missing tty driver or unsupported ioctl is logged, not treated as a successful
wake. Awakened screen initialization detaches kernel interfaces 0/1 as needed,
claims interface 1 and asserts CDC DTR/RTS on interface 0.

The installer generates `$HOME/.local/share/sama-display/70-sama-display.rules`
with the installing user's numeric UID, narrow VID/PID+serial matches for USB
and tty nodes, and mode 0660. It does **not** write `/etc` or escalate
privileges. Review and run separately as an administrator:

```sh
sudo install -m 0644 "$HOME/.local/share/sama-display/70-sama-display.rules" /etc/udev/rules.d/70-sama-display.rules
sudo udevadm control --reload-rules
# Replug the device or arrange a targeted udev trigger, after stopping the app.
```

No world-writable rule or broad USB permissions are required. This is a
single-owner machine-specific UID rule; regenerate it for another UID.

## User service

The installer copies the release binary to the stable
`~/.local/share/sama-display/bin/sama-display` path and copies the unit to
`~/.config/systemd/user/sama-display.service`. It deliberately does not invoke
systemctl or enable the unit:

```sh
systemctl --user daemon-reload
systemctl --user enable --now sama-display.service
journalctl --user -u sama-display.service -f
systemctl --user stop sama-display.service
```

The unit uses `default.target`, `Restart=on-failure`, five-second restart delay,
and five-second stop budget. A user manager starts at login; boot without login
requires administrator-managed lingering (`loginctl enable-linger USER`). This
installer does not change lingering. SIGTERM/SIGINT interrupt waits within
≤200 ms, reads poll at ≤200 ms and writes have a ≤500 ms timeout per 16000-byte
chunk; `nvidia-smi` has a two-second budget. Interfaces are released and
previously detached kernel drivers are reattached best-effort, including on
hotplug/errors. No media/fan/pump commands are sent during cleanup. Do not run
two instances; a state-directory advisory lock prevents duplicate service or
manual runs sharing a state directory.

## Protocol and state

HELLO waits up to eight seconds for the complete supported ROM identity,
STOP79/STOP96 wait up to eight seconds for `media_stop`. Bounded drain operations
remove queued identity replies and stale ACKs before frames. Full BGRA frames
are encoded as the observed 250-byte protocol blocks and written in ≤16000-byte
chunks. After PRE86, comma preamble and full-frame transfer, one QUERY is sent.
The receiver polls for **`full_png_sucess` or `full_png_success`** for up to
twelve seconds, including split replies and empty polls. Generic `success` is
not accepted. An empty poll never retransmits the frame. Errors discard the
session, release USB, back off, rediscover and handshake before the next
attempted frame. The firmware provides no sequence correlation; draining and one
in-flight frame are conservative protections, not a protocol-level transaction
ID guarantee.

Default state directory is `${XDG_STATE_HOME:-$HOME/.local/state}/sama-display`;
the installed unit explicitly selects `~/.local/state/sama-display`. Each ACK
publishes `status.json` and the rotated `latest-frame.png`, using same-directory
temporary files, flush/fsync and atomic replacement individually. The two files
are not a cross-file transaction. Status records persisted `sequence`, UTC
timestamps for start/sample/ACK, transfer-plus-ACK latency, full cycle latency,
CPU/GPU temperatures, rotation, ROM identity and ACK text. Only acknowledged
frames update these files. Sequence continues from a prior valid status; failed
frames leave the last acknowledged status available, so check its timestamp for
freshness. State/preview files never belong in source.

## Migrating from the Python prototype

The Python implementation is removed from this directory. A previously
installed Python runtime under `~/.local/share/sama-display` (venv, script and
requirements) is left untouched by the Rust installer so rollback stays
possible. After Rust live verification, remove that old runtime deliberately;
it is not used by the Rust unit. To roll back before removal, restore the prior
unit's `ExecStart` and restart the service.

## Offline validation

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
./install.sh --dry-run
```

Tests use injected transports, clocks, sensors and temporary state; they never
access USB. They cover the exact 250-byte wire encoding against Python golden
digests, split/delayed acknowledgements, timeout bounds, handshake command order,
single in-flight frames, reconnect cadence, cancellation release, rendering and
CCW rotation, atomic publication failure, CLI validation, bounded subprocesses,
bridge identity matching, chunked writes and the HOME-prefixed installer.
Hardware installation, live sensor/display integration, hotplug and reboot
behavior must be verified separately; no test result claims real hardware was
exercised by this task.
