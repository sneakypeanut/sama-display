#!/bin/sh
# Install the user application only; never escalate or enable a service.
set -eu

usage() {
    cat <<'EOF'
Usage: install.sh [--dry-run] [--binary PATH]

Installs the compiled sama-display binary and its user unit at stable
home-relative paths, and generates the exact-identity UID udev rule.

  --dry-run      print the plan without writing files or running commands
  --binary PATH  install PATH instead of target/release/sama-display

The installer does not use sudo, does not enable or start systemd units, and
does not remove a previously installed Python runtime.
EOF
}

DRY_RUN=0
BINARY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --binary)
            shift
            [ $# -gt 0 ] || { echo "--binary requires a path" >&2; exit 2; }
            BINARY=$1
            ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

SOURCE=$(dirname -- "$0")
SOURCE=$(CDPATH='' cd -- "$SOURCE" && pwd)
: "${HOME:?HOME must be set}"
UID_NUM=$(id -u)
if [ "$UID_NUM" -eq 0 ]; then
    echo "Run the user installer without root; install the generated udev rule separately." >&2
    exit 1
fi

APP="$HOME/.local/share/sama-display"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/sama-display.service"
RULE="$APP/70-sama-display.rules"
BIN="$APP/bin/sama-display"
[ -n "$BINARY" ] || BINARY="$SOURCE/target/release/sama-display"

if [ "$DRY_RUN" -eq 1 ]; then
    printf 'dry-run: no files written and no commands executed\n'
    printf 'home=%s\n' "$HOME"
    printf 'app=%s\n' "$APP"
    printf 'binary=%s\n' "$BINARY"
    printf 'install=%s\n' "$BIN"
    printf 'unit=%s\n' "$UNIT"
    printf 'udev_rule=%s\n' "$RULE"
    printf 'uid=%s\n' "$UID_NUM"
    printf 'service_activation=%s\n' "not performed"
    printf 'privilege_escalation=%s\n' "not performed"
    exit 0
fi

if [ ! -x "$BINARY" ]; then
    echo "binary not found or not executable: $BINARY" >&2
    echo "Build it first with: cargo build --release --locked" >&2
    exit 1
fi
for asset in README.md sama-display.service 70-sama-display.rules.in; do
    [ -f "$SOURCE/$asset" ] || { echo "missing installer asset: $SOURCE/$asset" >&2; exit 1; }
done

mkdir -p "$APP/bin" "$UNIT_DIR"
install -m 0755 "$BINARY" "$BIN"
install -m 0644 "$SOURCE/README.md" "$APP/README.md"
install -m 0644 "$SOURCE/sama-display.service" "$UNIT"
sed "s/@UID@/$UID_NUM/g" "$SOURCE/70-sama-display.rules.in" > "$RULE.tmp"
chmod 0644 "$RULE.tmp"
mv "$RULE.tmp" "$RULE"

printf 'installed=%s\n' "$BIN"
printf 'unit=%s\n' "$UNIT"
printf 'udev_rule=%s\n' "$RULE"
printf 'next=%s\n' "review and install the udev rule as administrator, then enable the user unit"
printf 'python_runtime=%s\n' "left untouched"
