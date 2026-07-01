#!/usr/bin/env bash
# Builds the Foxy Linux installer using the same layout as the GitHub workflow.
# Requirements:
#   - Rust toolchain with the selected Linux target
#   - Native Linux build dependencies listed in README.md

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo was not found in PATH." >&2
    exit 1
fi

APP_VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$APP_VERSION" ]; then
    echo "Error: Could not read package version from Cargo.toml." >&2
    exit 1
fi
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
TARGET_NAME="${TARGET%%-unknown-linux-gnu}"
OUTPUT="dist/Foxy-${APP_VERSION}-linux-${TARGET_NAME}-installer.sh"

mkdir -p dist

echo "[1/2] Building Linux release for $TARGET..."
cargo build --release --target "$TARGET"

echo "[2/2] Building Linux installer..."
chmod +x installer/linux/build-installer.sh
installer/linux/build-installer.sh \
    "target/$TARGET/release/Foxy" \
    src/ui/icons/foxy_256.png \
    "$OUTPUT" \
    "$APP_VERSION"

echo
echo "Built installer: $OUTPUT"
