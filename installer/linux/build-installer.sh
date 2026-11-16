#!/bin/bash
set -e

FOXY_BIN="$1"
ICON_PNG="$2"
OUTPUT="$3"
VERSION="${4:-dev}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HEADER="$SCRIPT_DIR/installer-header.sh"
DESKTOP_TEMPLATE="$SCRIPT_DIR/foxy.desktop"

if [ -z "$FOXY_BIN" ] || [ -z "$ICON_PNG" ] || [ -z "$OUTPUT" ]; then
    echo "Usage: $0 <foxy-binary> <icon-png> <output-installer> [version]"
    exit 1
fi

if [ ! -f "$FOXY_BIN" ]; then
    echo "Error: Foxy binary not found: $FOXY_BIN"
    exit 1
fi

if [ ! -f "$ICON_PNG" ]; then
    echo "Error: Icon not found: $ICON_PNG"
    exit 1
fi

if [ ! -f "$HEADER" ]; then
    echo "Error: Installer header not found: $HEADER"
    exit 1
fi

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

cp "$FOXY_BIN" "$TMPDIR/Foxy"
chmod +x "$TMPDIR/Foxy"
cp "$ICON_PNG" "$TMPDIR/foxy.png"

# The binary's $ORIGIN rpath requires libsteam_api.so beside it at runtime.
STEAM_LIB="$(dirname "$FOXY_BIN")/libsteam_api.so"
if [ ! -f "$STEAM_LIB" ]; then
    STEAM_LIB="$(find "$(dirname "$FOXY_BIN")/build" -name libsteam_api.so 2>/dev/null | head -n 1)"
fi
if [ ! -f "$STEAM_LIB" ]; then
    echo "Error: libsteam_api.so not found next to $FOXY_BIN or under its build directory"
    exit 1
fi
cp "$STEAM_LIB" "$TMPDIR/libsteam_api.so"

if [ -f "$DESKTOP_TEMPLATE" ]; then
    cp "$DESKTOP_TEMPLATE" "$TMPDIR/foxy.desktop"
fi

mkdir -p "$(dirname "$OUTPUT")"

cat "$HEADER" > "$OUTPUT"
tar czf - -C "$TMPDIR" . >> "$OUTPUT"
chmod +x "$OUTPUT"

SIZE=$(stat --printf="%s" "$OUTPUT" 2>/dev/null || stat -f "%z" "$OUTPUT" 2>/dev/null)
echo "Built installer: $OUTPUT ($SIZE bytes, version $VERSION)"
