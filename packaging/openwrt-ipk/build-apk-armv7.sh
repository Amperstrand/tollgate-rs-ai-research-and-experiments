#!/usr/bin/env bash
# build-apk-armv7.sh — arm_cortex-a7_neon-vfpv4 (ipq40xx et al.) .apk for
# OpenWrt 25.12+.
#
# Unlike MIPS (issue #94, tier 3, no prebuilt std), armv7 musl is a Rust
# tier-2 target: `rustup target add armv7-unknown-linux-musleabihf` ships
# prebuilt std, so plain cargo-zigbuild works on the host — no SDK
# container, no -Z build-std. The package arch is passed through verbatim
# (arm_cortex-a7_neon-vfpv4 — ipq40xx's packages arch; NOT the `arm`
# alias, which maps to arm_cortex-a7 and would be rejected by apk on
# ipq40xx devices).
#
# Verified end-to-end 2026-11-18: zero flag iteration, build 2m18s —
# ring 0.17.14 compiles for ARM32 because zig cc defines __ARM_ARCH (old
# GNU target triples without the armv7 subtype don't), libsqlite3-sys
# bundles sqlite3 (cc), secp256k1 via cc — and the resulting binary boots
# under qemu-arm-static (serve on 127.0.0.1:4814 with TOLLGATE_API_KEY,
# /admin/api/status 200, /v1/sessions/unknown 404). Output is a fully
# static ARM EABI5 hard-float ELF (no PT_INTERP, no NEEDED) — nothing
# beyond the kernel's armv7 support is required on the device.
#
# Requirements: rustup + cargo-zigbuild (zig via PATH or the `ziglang`
# pip package), apk-tools 3 + fakeroot for packaging (same as
# build-apk.sh); optionally qemu-arm-static on the host for the smoke.
#
# Usage:
#   APK=/path/to/apk ./build-apk-armv7.sh
#   TOLLGATE_FEATURES=v1-compat,spilman ./build-apk-armv7.sh
#   PKG_VERSION=0.1.0 ./build-apk-armv7.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

RUST_TARGET="armv7-unknown-linux-musleabihf"
# Must byte-for-byte match the target's package arch (ipq40xx:
# arm_cortex-a7_neon-vfpv4 — the underscore form passes through
# build-apk.sh's arch table unchanged).
OPENWRT_ARCH="arm_cortex-a7_neon-vfpv4"
FEATURES="${TOLLGATE_FEATURES:-v1-compat}"

command -v cargo-zigbuild >/dev/null 2>&1 || {
    echo "error: cargo-zigbuild not found (cargo install cargo-zigbuild)" >&2
    exit 1
}

echo "==> Adding Rust target $RUST_TARGET (tier-2, prebuilt std)..."
rustup target add "$RUST_TARGET" >/dev/null

echo "==> Cross-compiling (cargo zigbuild, zig cc for the C bits)..."
cd "$PROJECT_ROOT"
cargo zigbuild --release --features "$FEATURES" \
    --target "$RUST_TARGET" --bin tollgate --bin tolltop

# zigbuild emits already-stripped release binaries; locate them (the
# default is target/<triple>/release, but a host-wide CARGO_TARGET_DIR /
# build.target-dir pin moves it — resolve via cargo metadata then).
BIN_DIR="$PROJECT_ROOT/target/$RUST_TARGET/release"
if [ ! -f "$BIN_DIR/tollgate" ]; then
    BIN_DIR="$(cargo metadata --format-version 1 --no-deps \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/$RUST_TARGET/release"
fi

# Smoke: --version proves the ELF loads and the target triple is sane.
if command -v qemu-arm-static >/dev/null 2>&1; then
    qemu-arm-static "$BIN_DIR/tollgate" --version
    qemu-arm-static "$BIN_DIR/tolltop" --version
else
    echo "    note: qemu-arm-static not found — skipping the emulated --version smoke"
fi

echo "==> Packaging .apk..."
exec "$SCRIPT_DIR/build-apk.sh" --arch "$OPENWRT_ARCH" --bin-dir "$BIN_DIR"
