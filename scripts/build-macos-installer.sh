#!/usr/bin/env bash
# Builds an experimental local macOS Apple Silicon dmg for Foxy.
# Requirements:
#   - macOS with a Rust toolchain that can build aarch64-apple-darwin
#   - hdiutil, provided by macOS

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo was not found in PATH." >&2
    exit 1
fi

if ! command -v hdiutil >/dev/null 2>&1; then
    echo "Error: hdiutil was not found in PATH. Build this installer on macOS." >&2
    exit 1
fi

APP_VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$APP_VERSION" ]; then
    echo "Error: Could not read package version from Cargo.toml." >&2
    exit 1
fi
TARGET="aarch64-apple-darwin"
APP_NAME="Foxy"
BINARY="target/$TARGET/release/Foxy"
OUTPUT="dist/Foxy-${APP_VERSION}-macos-arm64.dmg"

mkdir -p dist

echo "[1/2] Building macOS release for $TARGET..."
cargo build --release --target "$TARGET"

echo "[2/2] Building macOS dmg..."
if [ ! -f "$BINARY" ]; then
    echo "Error: Foxy binary not found: $BINARY" >&2
    exit 1
fi

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

DMG_ROOT="$TMPDIR/dmg-root"
APP_BUNDLE="$DMG_ROOT/$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"

mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BINARY" "$CONTENTS/MacOS/$APP_NAME"
chmod +x "$CONTENTS/MacOS/$APP_NAME"

cat > "$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>community.foxy.Foxy</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$APP_VERSION</string>
    <key>CFBundleVersion</key>
    <string>$APP_VERSION</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSRequiresNativeExecution</key>
    <true/>
</dict>
</plist>
EOF

ln -s /Applications "$DMG_ROOT/Applications"
rm -f "$OUTPUT"
hdiutil create \
    -volname "$APP_NAME $APP_VERSION" \
    -srcfolder "$DMG_ROOT" \
    -ov \
    -format UDZO \
    "$OUTPUT"

echo
echo "Built installer: $OUTPUT"
