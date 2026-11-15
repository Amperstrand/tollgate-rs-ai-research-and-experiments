#!/usr/bin/env bash
# build-apk.sh — build an OpenWrt 25.12+ (.apk / apk-tools v3) package.
#
# OpenWrt 25.12 dropped opkg entirely; .ipk files fail with
# "v2 package format error" (verified on 25.12.6, 2026-11-15). This wraps
# `apk mkpkg` from apk-tools 3 (https://gitlab.alpinelinux.org/alpine/apk-tools):
#
#   git clone --depth 1 https://gitlab.alpinelinux.org/alpine/apk-tools.git
#   meson setup build && ninja -C build
#   APK=~/apk-tools/build/src/apk ./build-apk.sh --arch x86_64 --bin-dir bin/
#
# The package is unsigned; install with `apk add --allow-untrusted` (or
# sign it with your keys for real distribution).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FILES_DIR="$SCRIPT_DIR/files"

APK="${APK:-apk}"
ARCH="x86_64"
BIN_DIR=""
PKG_NAME="tollgate"
PKG_VERSION="${PKG_VERSION:-$(cd "$PROJECT_ROOT" && git describe --tags --always --dirty 2>/dev/null || echo "0.1.0")}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --arch=*) ARCH="${1#*=}"; shift ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

command -v "$APK" >/dev/null 2>&1 || {
    echo "error: apk-tools 3 not found (set APK=/path/to/apk)" >&2
    exit 1
}

# apk versions must match x.y.z[-rN]; sanitize anything else (e.g. the
# git-describe default or an ipk-style 0.1.0-vm25) into that shape.
apk_sanitize_version() {
    local v="$1" base rel
    base="${v%%-*}"
    rel="${v#*-}"
    case "$base" in
        *[0-9]*) ;;  # base looks like a version; keep it
        *) base="1.0.0" ;;  # no digits at all — fallback
    esac
    if [ "$rel" = "$v" ] || [ -z "$rel" ]; then
        printf '%s-r1' "$base"
    else
        # keep digits only; fall back to a commit-count-ish default
        rel="$(echo "$rel" | tr -cd '0-9')"
        printf '%s-r%s' "$base" "${rel:-1}"
    fi
}
PKG_VERSION="$(apk_sanitize_version "$PKG_VERSION")"

if [ -n "$BIN_DIR" ]; then
    for bin in tollgate tolltop; do
        [ -f "$BIN_DIR/$bin" ] || { echo "error: $BIN_DIR/$bin missing" >&2; exit 1; }
    done
else
    echo "error: --bin-dir is required (prebuilt tollgate + tolltop)" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/stage"
DIST_DIR="$PROJECT_ROOT/dist"
mkdir -p "$STAGE/usr/bin" "$STAGE/www" "$STAGE/etc/tollgate" "$DIST_DIR"

install -m 0755 "$BIN_DIR/tollgate" "$BIN_DIR/tolltop" "$STAGE/usr/bin/"
cp -a "$FILES_DIR/etc" "$STAGE/"
if [ -d "$FILES_DIR/www/tollgate-captive-portal-site" ]; then
    cp -a "$FILES_DIR/www/tollgate-captive-portal-site" "$STAGE/www/"
fi

OUT="$DIST_DIR/${PKG_NAME}-${PKG_VERSION}.apk"
"$APK" mkpkg -F "$STAGE" -o "$OUT" \
    --info "name:$PKG_NAME" \
    --info "version:$PKG_VERSION" \
    --info "arch:$ARCH" \
    --info "description:TollGate - Cashu-metered network access" \
    --info "license:MIT" \
    --info "depends:ip-full"

echo "==> $OUT ($(du -h "$OUT" | cut -f1))"
echo "    Install: apk add --allow-untrusted $OUT"
