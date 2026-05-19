#!/usr/bin/env bash
# Build a tollgate-wrt .ipk locally (mirrors CI package-ipk staging).
#
# Usage:
#   ./scripts/build-ipk-local.sh [openwrt_arch] [rust_target]
#
# Examples:
#   ./scripts/build-ipk-local.sh mipsel_24kc mipsel-unknown-linux-gnu
#   ./scripts/build-ipk-local.sh x86_64 x86_64-unknown-linux-musl
#   ./scripts/build-ipk-local.sh aarch64_cortex-a53 aarch64-unknown-linux-musl
#
# Requires: cross (cargo install cross), curl, python3

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ARCH="${1:-mipsel_24kc}"
TARGET="${2:-mipsel-unknown-linux-gnu}"
PACKAGE_NAME="${PACKAGE_NAME:-tollgate-wrt}"
BINARY_NAME="${BINARY_NAME:-tollgate-net}"
VERSION="${PKG_VERSION:-$(git rev-parse --short HEAD 2>/dev/null || echo dev)}"
OUT_DIR="${OUT_DIR:-$ROOT/artifacts}"
TOOLCHAIN="${CROSS_TOOLCHAIN:-}"

case "$TARGET" in
  mipsel-unknown-linux-gnu | mips-unknown-linux-gnu)
    TOOLCHAIN="${TOOLCHAIN:-nightly}"
    ;;
  *)
    TOOLCHAIN="${TOOLCHAIN:-stable}"
    ;;
esac

echo "==> Cross-compiling $BINARY_NAME for $TARGET (toolchain=$TOOLCHAIN)"
if [ "$TOOLCHAIN" = "nightly" ]; then
  rustup toolchain install nightly --target "$TARGET" 2>/dev/null || true
  cross +nightly build --release --target "$TARGET" --bin "$BINARY_NAME" --features nds
else
  rustup target add "$TARGET" 2>/dev/null || true
  cross build --release --target "$TARGET" --bin "$BINARY_NAME" --features nds
fi

BIN="target/$TARGET/release/$BINARY_NAME"
[ -f "$BIN" ] || { echo "error: binary not found at $BIN" >&2; exit 1; }

echo "==> Fetching captive portal site from Go v1 reference"
PORTAL_DIR="$(mktemp -d)"
trap 'rm -rf "$PORTAL_DIR"' EXIT
curl -sL "https://api.github.com/repos/OpenTollGate/tollgate-module-basic-go/git/trees/main?recursive=1" \
  | python3 -c "
import json, sys, urllib.request
data = json.load(sys.stdin)
for item in data.get('tree', []):
    p = item['path']
    if p.startswith('packaging/files/tollgate-captive-portal-site/') and item['type'] == 'blob':
        print(p.removeprefix('packaging/files/tollgate-captive-portal-site/'))
" | while read -r rel; do
    [ -n "$rel" ] || continue
    url="https://raw.githubusercontent.com/OpenTollGate/tollgate-module-basic-go/main/packaging/files/tollgate-captive-portal-site/$rel"
    dest="$PORTAL_DIR/$rel"
    mkdir -p "$(dirname "$dest")"
    curl -sL "$url" -o "$dest"
  done

echo "==> Staging IPK payload"
PAYLOAD="$(mktemp -d)"
trap 'rm -rf "$PAYLOAD" "$PORTAL_DIR"' EXIT
install -D -m 0755 "$BIN" "$PAYLOAD/usr/bin/tollgate-wrt"
install -D -m 0755 packaging/files/etc/init.d/tollgate-wrt "$PAYLOAD/etc/init.d/tollgate-wrt"
install -D -m 0644 packaging/files/etc/config/firewall-tollgate "$PAYLOAD/etc/config/firewall-tollgate"
install -D -m 0755 packaging/files/etc/hotplug.d/iface/95-tollgate-restart "$PAYLOAD/etc/hotplug.d/iface/95-tollgate-restart"
install -D -m 0644 packaging/files/lib/upgrade/keep.d/tollgate "$PAYLOAD/lib/upgrade/keep.d/tollgate"
install -D -m 0755 packaging/files/etc/uci-defaults/90-tollgate-captive-portal-symlink "$PAYLOAD/etc/uci-defaults/90-tollgate-captive-portal-symlink"
install -D -m 0755 packaging/files/etc/uci-defaults/99-tollgate-setup "$PAYLOAD/etc/uci-defaults/99-tollgate-setup"
mkdir -p "$PAYLOAD/etc/tollgate/ecash" "$PAYLOAD/etc/crontabs"
install -D -m 0644 LICENSE "$PAYLOAD/usr/share/doc/tollgate-wrt/LICENSE"
cp -a "$PORTAL_DIR/." "$PAYLOAD/etc/tollgate/tollgate-captive-portal-site/"

mkdir -p "$OUT_DIR"
OUTPUT="$OUT_DIR/${PACKAGE_NAME}_${VERSION}_${ARCH}.ipk"

echo "==> Building $OUTPUT"
env \
  PKG_NAME="$PACKAGE_NAME" \
  PKG_VERSION="$VERSION" \
  ARCH="$ARCH" \
  MAINTAINER="Amperstrand" \
  LICENSE="MIT" \
  DESCRIPTION="TollGate v2 — Rust payment router for OpenWrt" \
  DEPENDS="nodogsplash" \
  packaging/build-ipk.sh "$PAYLOAD" "$OUTPUT"

echo "Built: $OUTPUT ($(wc -c < "$OUTPUT" | tr -d ' ') bytes)"
