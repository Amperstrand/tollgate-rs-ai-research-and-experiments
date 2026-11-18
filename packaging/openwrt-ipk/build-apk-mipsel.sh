#!/usr/bin/env bash
# build-apk-mipsel.sh — mipsel_24kc (ramips/mt7621) .apk for OpenWrt 25.12+.
#
# The standalone cargo-zigbuild path cannot build MIPS (rust 1.99 has no
# prebuilt std for any mips target — tier 3; zig fails on both endians;
# see issue #94). This script takes the remaining viable path: the OpenWrt
# SDK's musl cross-toolchain + nightly `-Z build-std`, both digest/version
# pinned, then hands the binaries to build-apk.sh for packaging.
#
# Verified end-to-end 2026-11-18 (issue #94): the build finished in 2m48s
# with zero flag iteration — ring 0.17.14 compiles for mipsel via its
# portable-C source set, libsqlite3-sys bundles sqlite3 (cc), secp256k1
# builds via cc — and the resulting binary boots under qemu-mipsel-static
# (serve + /admin/api/status 200 + /v1/sessions/unknown 404).
#
# Requirements: docker (build runs inside the pinned SDK container);
# apk-tools 3 + fakeroot for packaging (same as build-apk.sh); optionally
# qemu-mipsel-static on the host for the smoke check.
#
# Usage:
#   APK=/path/to/apk ./build-apk-mipsel.sh
#   TOLLGATE_FEATURES=v1-compat,spilman ./build-apk-mipsel.sh
#   PKG_VERSION=0.1.0 ./build-apk-mipsel.sh
#
# All state is cached in docker volumes (rustup + cargo), keyed by the
# pinned toolchain — never point CARGO_HOME at a shared host cache.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# openwrt/sdk:ramips-mt7621-25.12.0 — digest-pinned (same pin discipline as
# the tbmg build-inputs.json SDK table; bump deliberately, never floating.
SDK_IMAGE="${SDK_IMAGE:-openwrt/sdk@sha256:000f3add430c8d285e4ae7e7569f06d9143c48df8a0a03bf15891ece8d3dae0f}"
# Nightly that ships rust-src and accepts -Z build-std (1.102.0-nightly).
RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-nightly-2026-11-08}"
FEATURES="${TOLLGATE_FEATURES:-v1-compat}"
RUST_TARGET="mipsel-unknown-linux-musl"

command -v docker >/dev/null 2>&1 || {
    echo "error: docker not found (the cross-build runs in the SDK container)" >&2
    exit 1
}

echo "==> Pulling SDK image (digest-pinned)..."
docker pull "$SDK_IMAGE" >/dev/null

VOL_SUFFIX="$(echo "$RUST_TOOLCHAIN" | tr '/.' '__')"
RUSTUP_VOL="tollgate-mipsel-rustup-${VOL_SUFFIX}"
CARGO_VOL="tollgate-mipsel-cargo-${VOL_SUFFIX}"
docker volume create "$RUSTUP_VOL" >/dev/null
docker volume create "$CARGO_VOL" >/dev/null

# Mount the host's qemu-user-static (a static binary — runs anywhere) into
# the container for the smoke check; harmless when absent.
QEMU_MOUNT=()
if command -v qemu-mipsel-static >/dev/null 2>&1; then
    QEMU_MOUNT=(-v "$(command -v qemu-mipsel-static):/usr/local/bin/qemu-mipsel-static:ro")
else
    echo "    note: qemu-mipsel-static not found — skipping the emulated --version smoke"
fi

echo "==> Cross-compiling (this includes build-std; first run downloads the toolchain)..."
# -i is required: the inner script arrives on stdin (heredoc).
# --user 0:0: the image's default USER (buildbot) cannot write the fresh
# docker volumes (root-owned); the chown at the end hands /src/target back.
docker run --rm -i --user 0:0 --entrypoint bash \
    -v "$PROJECT_ROOT":/src -w /src \
    -v "$RUSTUP_VOL":/rustup \
    -v "$CARGO_VOL":/cargo \
    "${QEMU_MOUNT[@]}" \
    -e RUSTUP_HOME=/rustup -e CARGO_HOME=/cargo \
    -e RUST_TOOLCHAIN="$RUST_TOOLCHAIN" -e FEATURES="$FEATURES" \
    "$SDK_IMAGE" -euo pipefail <<'INNER'
TC_BIN="$(echo /builder/staging_dir/toolchain-mipsel_24kc_*/bin)"
SYSROOT="$(dirname "$TC_BIN")"
export PATH="$TC_BIN:$PATH"
export STAGING_DIR=/builder/staging_dir
export CC_mipsel_unknown_linux_musl=mipsel-openwrt-linux-musl-gcc
export AR_mipsel_unknown_linux_musl=mipsel-openwrt-linux-musl-gcc-ar
export CARGO_TARGET_MIPSEL_UNKNOWN_LINUX_MUSL_LINKER=mipsel-openwrt-linux-musl-gcc

if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain "$RUST_TOOLCHAIN" --profile minimal >/dev/null
fi
PATH="$CARGO_HOME/bin:$PATH"
rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rust-src >/dev/null

cargo "+$RUST_TOOLCHAIN" build -Z build-std=std,panic_abort \
    --release --target mipsel-unknown-linux-musl \
    --features "$FEATURES" --bin tollgate --bin tolltop

BIN_DIR="/src/target/mipsel-unknown-linux-musl/release"
mipsel-openwrt-linux-musl-strip "$BIN_DIR/tollgate" "$BIN_DIR/tolltop"
chown -R "$(stat -c '%u:%g' /src)" /src/target

# Smoke: the musl interpreter lives in the SDK sysroot (ld-musl-mipsel-sf.so.1
# -> libc.so), so -L resolves it. A passing --version proves the ELF
# interpreter, cpu flags and dynamic deps are all mipsel_24kc-correct.
if command -v qemu-mipsel-static >/dev/null 2>&1; then
    qemu-mipsel-static -L "$SYSROOT" "$BIN_DIR/tollgate" --version
    qemu-mipsel-static -L "$SYSROOT" "$BIN_DIR/tolltop" --version
fi
INNER

echo "==> Packaging .apk..."
BIN_DIR="$PROJECT_ROOT/target/$RUST_TARGET/release"
exec "$SCRIPT_DIR/build-apk.sh" --arch mipsel --bin-dir "$BIN_DIR"
