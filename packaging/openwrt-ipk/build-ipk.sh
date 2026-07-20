#!/bin/bash
# Build a TollGate .ipk package for OpenWrt without the OpenWrt SDK.
#
# Uses cargo-zigbuild for cross-compilation and assembles the .ipk directly.
# An OpenWrt .ipk is a gzip-compressed tar archive containing:
#   ./debian-binary   — format version ("2.0\n")
#   ./control.tar.gz  — package metadata, conffiles, pre/post scripts
#   ./data.tar.gz     — the actual filesystem tree
# (Debian .deb uses ar; OpenWrt .ipk uses tar.gz. NOT an ar archive.)
#
# Usage:
#   ./packaging/openwrt-ipk/build-ipk.sh [--arch <name>] [--features <list>] [--bin-dir <dir>]
#
# Architectures (--arch):
#   aarch64   GL.iNet MT3000/MT6000, RPi 3/4/5, most modern routers  [default]
#   mipsel    Older MIPS routers (TP-Link, Netgear, GL.iNet AR750)
#   mips      MIPS big-endian routers (ath79)
#   arm       32-bit ARM routers (Cortex-A7)
#   x86_64    x86 routers / VMs
#
# Output: dist/tollgate_<version>_<openwrt-arch>.ipk
#
# Environment:
#   TOLLGATE_FEATURES  cargo --features list (default: v1-compat,spilman)
#   PKG_VERSION        override version (default: git describe)
#   SOURCE_DATE_EPOCH  reproducible-build mtime (optional)
#   LLVM_STRIP         strip binary override (default: strip)
#
# Prerequisites:
#   cargo install cargo-zigbuild
#   rustup target add <rust-triple>   (added automatically if missing)

set -euo pipefail

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------

ARCH="aarch64"
FEATURES="${TOLLGATE_FEATURES:-v1-compat}"
BIN_DIR=""   # if set, use prebuilt binaries from here instead of compiling

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --arch=*) ARCH="${1#*=}"; shift ;;
        --features) FEATURES="$2"; shift 2 ;;
        --features=*) FEATURES="${1#*=}"; shift ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Architecture mapping
#
# RUST_TARGET   — passed to cargo --target
# OPENWRT_ARCH  — goes in the .ipk control file and filename
# ---------------------------------------------------------------------------

case "$ARCH" in
    aarch64)
        RUST_TARGET="aarch64-unknown-linux-musl"
        OPENWRT_ARCH="aarch64_cortex-a53"
        ;;
    mipsel)
        RUST_TARGET="mipsel-unknown-linux-musl"
        OPENWRT_ARCH="mipsel_24kc"
        MIPS_TIER3=1
        ;;
    mips)
        RUST_TARGET="mips-unknown-linux-musl"
        OPENWRT_ARCH="mips_24kc"
        MIPS_TIER3=1
        ;;
    arm)
        RUST_TARGET="arm-unknown-linux-musleabihf"
        OPENWRT_ARCH="arm_cortex-a7"
        ;;
    x86_64)
        RUST_TARGET="x86_64-unknown-linux-musl"
        OPENWRT_ARCH="x86_64"
        ;;
    *)
        echo "Unknown arch: $ARCH" >&2
        echo "Valid: aarch64, mipsel, mips, arm, x86_64" >&2
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FILES_DIR="$SCRIPT_DIR/files"
DIST_DIR="$PROJECT_ROOT/dist"

PKG_NAME="tollgate"
PKG_VERSION="${PKG_VERSION:-$(cd "$PROJECT_ROOT" && git describe --tags --always --dirty 2>/dev/null || echo "0.1.0")}"

echo "==> Building $PKG_NAME $PKG_VERSION for $OPENWRT_ARCH ($RUST_TARGET)"
echo "    features: $FEATURES"

if [ "${MIPS_TIER3:-0}" = "1" ] && [ -z "$BIN_DIR" ]; then
    echo "" >&2
    echo "ERROR: $RUST_TARGET is a Rust Tier 3 target — no pre-compiled std available." >&2
    echo "       cargo-zigbuild cannot produce a standalone binary for MIPS." >&2
    echo "" >&2
    echo "Options:" >&2
    echo "  1. Use the OpenWrt SDK (Makefile) — rust/host builds std from source" >&2
    echo "  2. Provide prebuilt binaries: --bin-dir path/to/mips/release" >&2
    echo "  3. Use nightly + -Z build-std (experimental): cargo +nightly build -Z build-std" >&2
    echo "" >&2
    echo "See packaging/openwrt-ipk/README.md for details." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Obtain binaries
#
# Either use a directory of prebuilt binaries (--bin-dir; CI cross-compiles
# once in a shared job and hands them to the packager), or compile from source
# here for a self-contained local build.
# ---------------------------------------------------------------------------

if [ -n "$BIN_DIR" ]; then
    RELEASE_DIR="$BIN_DIR"
    echo "==> Using prebuilt binaries from $RELEASE_DIR"
    for bin in tollgate tolltop; do
        [ -f "$RELEASE_DIR/$bin" ] || {
            echo "Error: prebuilt binary not found: $RELEASE_DIR/$bin" >&2
            exit 1
        }
    done
else
    if ! command -v cargo-zigbuild &>/dev/null; then
        echo "Error: cargo-zigbuild not found." >&2
        echo "  Install: cargo install cargo-zigbuild" >&2
        exit 1
    fi

    if ! rustup target list --installed 2>/dev/null | grep -q "^$RUST_TARGET$"; then
        echo "==> Adding Rust target $RUST_TARGET..."
        rustup target add "$RUST_TARGET"
    fi

    echo "==> Compiling..."
    cd "$PROJECT_ROOT"
    cargo zigbuild \
        --release \
        --target "$RUST_TARGET" \
        --features "$FEATURES" \
        --bin tollgate \
        --bin tolltop

    RELEASE_DIR="$PROJECT_ROOT/target/$RUST_TARGET/release"

    echo "==> Stripping binaries..."
    STRIP="${LLVM_STRIP:-strip}"
    for bin in tollgate tolltop; do
        "$STRIP" "$RELEASE_DIR/$bin" 2>/dev/null || true
    done
fi

SIZE=$(du -sh "$RELEASE_DIR/tollgate" | cut -f1)
echo "    tollgate: $SIZE"

# ---------------------------------------------------------------------------
# 2. Assemble .ipk
# ---------------------------------------------------------------------------

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

CONTROL_DIR="$WORK_DIR/control"
DATA_DIR="$WORK_DIR/data"
mkdir -p "$CONTROL_DIR" "$DATA_DIR"

# ---- data tree ----

install -d "$DATA_DIR/usr/bin"
install -m 0755 "$RELEASE_DIR/tollgate" "$DATA_DIR/usr/bin/tollgate"
install -m 0755 "$RELEASE_DIR/tolltop"  "$DATA_DIR/usr/bin/tolltop"

install -d "$DATA_DIR/etc/init.d"
install -m 0755 "$FILES_DIR/etc/init.d/tollgate" "$DATA_DIR/etc/init.d/tollgate"

install -d "$DATA_DIR/etc/tollgate"
# Install the example config as the live config; marked as a conffile below so
# opkg preserves user edits across upgrades.
install -m 0600 "$FILES_DIR/etc/tollgate/tollgate.yaml.example" \
    "$DATA_DIR/etc/tollgate/tollgate.yaml"

install -d "$DATA_DIR/etc/config"
install -m 0644 "$FILES_DIR/etc/config/tollgate"          "$DATA_DIR/etc/config/tollgate"
install -m 0644 "$FILES_DIR/etc/config/firewall-tollgate" "$DATA_DIR/etc/config/firewall-tollgate"

install -d "$DATA_DIR/etc/hotplug.d/iface"
install -m 0755 "$FILES_DIR/etc/hotplug.d/iface/95-tollgate-restart" \
    "$DATA_DIR/etc/hotplug.d/iface/95-tollgate-restart"

install -d "$DATA_DIR/etc/uci-defaults"
install -m 0755 "$FILES_DIR/etc/uci-defaults/90-tollgate-firewall" \
    "$DATA_DIR/etc/uci-defaults/90-tollgate-firewall"
install -m 0755 "$FILES_DIR/etc/uci-defaults/99-tollgate-setup" \
    "$DATA_DIR/etc/uci-defaults/99-tollgate-setup"

install -d "$DATA_DIR/lib/upgrade/keep.d"
install -m 0644 "$FILES_DIR/lib/upgrade/keep.d/tollgate" \
    "$DATA_DIR/lib/upgrade/keep.d/tollgate"

# ---- control files ----

PKG_SIZE=$(du -sk "$DATA_DIR" | cut -f1)

cat > "$CONTROL_DIR/control" <<EOF
Package: $PKG_NAME
Version: $PKG_VERSION
Architecture: $OPENWRT_ARCH
Maintainer: TollGate contributors
Section: net
Priority: optional
Depends: nftables, ip-full, kmod-nft-tproxy
Description: TollGate - metered network access with Cashu micropayments
 Rust implementation of the TollGate protocol. Sells metered network access
 over IP using Cashu ecash and Spilman payment channels. Includes the
 tollgate node daemon and the tolltop TUI dashboard.
Installed-Size: $PKG_SIZE
EOF

# Mark tollgate.yaml as a conffile so opkg won't overwrite user edits on upgrade.
cat > "$CONTROL_DIR/conffiles" <<EOF
/etc/tollgate/tollgate.yaml
EOF

cat > "$CONTROL_DIR/postinst" <<'EOF'
#!/bin/sh
# Run first-boot UCI defaults (each script deletes itself on success).
for s in /etc/uci-defaults/9[0-9]-tollgate-*; do
    [ -x "$s" ] || continue
    "$s" && rm -f "$s"
done

# Enable and start the service.
/etc/init.d/tollgate enable 2>/dev/null || true
/etc/init.d/tollgate start  2>/dev/null || true
exit 0
EOF
chmod 0755 "$CONTROL_DIR/postinst"

cat > "$CONTROL_DIR/prerm" <<'EOF'
#!/bin/sh
/etc/init.d/tollgate stop    2>/dev/null || true
/etc/init.d/tollgate disable 2>/dev/null || true
exit 0
EOF
chmod 0755 "$CONTROL_DIR/prerm"

# ---- pack ----

PKG_FILENAME="${PKG_NAME}_${PKG_VERSION}_${OPENWRT_ARCH}.ipk"
IPK_WORK="$WORK_DIR/ipk"
mkdir -p "$IPK_WORK"

echo "2.0" > "$IPK_WORK/debian-binary"

# Detect a tar that supports --format=gnu.
# On macOS, Homebrew's GNU tar is installed as 'gtar'; the system tar is BSD.
# Our filenames are short so BSD tar (ustar) works too, but gnu is preferred
# to match ipkg-build exactly and to embed numeric UID/GID.
# COPYFILE_DISABLE=1 suppresses macOS resource-fork (._*) files; no-op on Linux.
if command -v gtar &>/dev/null; then
    # Homebrew GNU tar on macOS
    TAR_CMD="gtar"
    TAR_EXTRA_FLAGS="--format=gnu --numeric-owner"
elif tar --version 2>/dev/null | grep -q 'GNU tar'; then
    # System tar is GNU tar (Linux)
    TAR_CMD="tar"
    TAR_EXTRA_FLAGS="--format=gnu --numeric-owner"
else
    # macOS BSD tar (libarchive). Its default format is PAX (typeflag 0x78),
    # which OpenWrt's busybox tar cannot handle. Force ustar explicitly.
    TAR_CMD="tar"
    TAR_EXTRA_FLAGS="--format=ustar"
fi

ipk_tar() {
    # ipk_tar <output.tar.gz> <source-dir> [paths...]
    local out="$1" src="$2"; shift 2
    local mtime_flags=""
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        mtime_flags="--mtime=@$SOURCE_DATE_EPOCH"
    fi
    COPYFILE_DISABLE=1 "$TAR_CMD" $TAR_EXTRA_FLAGS $mtime_flags -czf "$out" -C "$src" "$@"
}

ipk_tar "$IPK_WORK/control.tar.gz" "$CONTROL_DIR" .
ipk_tar "$IPK_WORK/data.tar.gz"    "$DATA_DIR"    .

# The outer .ipk container is a gzip-compressed tar — NOT an ar archive.
# (Debian .deb uses ar; OpenWrt .ipk uses tar.gz.)
# Entries must be named with ./ prefix, as ipkg-build produces.
mkdir -p "$DIST_DIR"
ipk_tar "$DIST_DIR/$PKG_FILENAME" "$IPK_WORK" ./debian-binary ./control.tar.gz ./data.tar.gz

echo ""
echo "==> Done: dist/$PKG_FILENAME"
echo "    $(du -sh "$DIST_DIR/$PKG_FILENAME" | cut -f1)"
echo ""
echo "Install on router:"
echo "    scp -O dist/$PKG_FILENAME root@192.168.1.1:/tmp/"
echo "    ssh root@192.168.1.1 opkg install /tmp/$PKG_FILENAME"
