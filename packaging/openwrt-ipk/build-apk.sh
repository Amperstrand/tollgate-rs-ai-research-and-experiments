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
#
# `fakeroot` is required: apk mkpkg records the staging tree's ownership
# verbatim, so staging as the build user would ship files owned by that
# user — on the router (no such user) they install as nobody:nogroup,
# leaving /etc/init.d/tollgate writable by nobody. The OpenWrt SDK wraps
# its own apk packaging in fakeroot for exactly this reason.
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
command -v fakeroot >/dev/null 2>&1 || {
    echo "error: fakeroot not found (required so the package ships root:root;" >&2
    echo "       apk mkpkg records the staging tree's ownership verbatim)" >&2
    exit 1
}

# Map the convenience arch aliases (same table as build-ipk.sh) to the
# OpenWrt package arch. A package built with arch:aarch64 is REJECTED by
# apk on every real OpenWrt aarch64 device (their arch is e.g.
# aarch64_cortex-a53 — verified with apk-tools 3.0.9: "error: uninstallable").
# Values already containing '_' are explicit OpenWrt arches and pass through.
case "$ARCH" in
    aarch64) OPENWRT_ARCH="aarch64_cortex-a53" ;;
    mipsel)  OPENWRT_ARCH="mipsel_24kc" ;;
    mips)    OPENWRT_ARCH="mips_24kc" ;;
    arm)     OPENWRT_ARCH="arm_cortex-a7" ;;
    x86_64)  OPENWRT_ARCH="x86_64" ;;
    *_*)     OPENWRT_ARCH="$ARCH" ;;
    *)
        echo "error: unknown arch '$ARCH' (valid: aarch64, mipsel, mips, arm, x86_64," >&2
        echo "       or an explicit OpenWrt arch like aarch64_cortex-a53)" >&2
        exit 1
        ;;
esac

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
mkdir -p "$DIST_DIR"
chmod 0755 "$WORK"

# Stage with explicit modes (a plain `cp -a` inherits the build host's
# umask — e.g. 0664 configs and 0775 dirs — which then ships verbatim).
install -d -m 0755 "$STAGE/usr/bin" "$STAGE/www" "$STAGE/etc/tollgate" \
    "$STAGE/etc/config" "$STAGE/etc/init.d" "$STAGE/etc/hotplug.d/iface" \
    "$STAGE/etc/uci-defaults" "$STAGE/lib/upgrade/keep.d"

install -m 0755 "$BIN_DIR/tollgate" "$BIN_DIR/tolltop" "$STAGE/usr/bin/"

install -m 0755 "$FILES_DIR/etc/init.d/tollgate" "$STAGE/etc/init.d/tollgate"
install -m 0755 "$FILES_DIR/etc/hotplug.d/iface/95-tollgate-restart" \
    "$STAGE/etc/hotplug.d/iface/95-tollgate-restart"
install -m 0755 "$FILES_DIR/etc/uci-defaults/90-tollgate-firewall" \
    "$STAGE/etc/uci-defaults/90-tollgate-firewall"
install -m 0755 "$FILES_DIR/etc/uci-defaults/99-tollgate-setup" \
    "$STAGE/etc/uci-defaults/99-tollgate-setup"
install -m 0644 "$FILES_DIR/etc/config/tollgate" "$STAGE/etc/config/tollgate"
install -m 0644 "$FILES_DIR/etc/config/firewall-tollgate" \
    "$STAGE/etc/config/firewall-tollgate"
install -m 0644 "$FILES_DIR/etc/tollgate/tollgate.yaml.example" \
    "$STAGE/etc/tollgate/tollgate.yaml.example"
# Install the example as the live config too (mirrors build-ipk.sh). apk's
# /etc protection keeps user edits across upgrades (the conffile analogue).
install -m 0600 "$FILES_DIR/etc/tollgate/tollgate.yaml.example" \
    "$STAGE/etc/tollgate/tollgate.yaml"
install -m 0644 "$FILES_DIR/lib/upgrade/keep.d/tollgate" \
    "$STAGE/lib/upgrade/keep.d/tollgate"

# Captive portal site (uhttpd serves /www with zero wiring).
if [ -d "$FILES_DIR/www/tollgate-captive-portal-site" ]; then
    cp -a "$FILES_DIR/www/tollgate-captive-portal-site" "$STAGE/www/"
    find "$STAGE/www" -type d -exec chmod 0755 {} +
    find "$STAGE/www" -type f -exec chmod 0644 {} +
fi

# post-install / pre-deinstall — mirror the .ipk postinst/prerm so an
# `apk add` on a running router behaves like `opkg install` did: run the
# first-boot uci-defaults immediately and enable + start the service.
cat > "$WORK/post-install" <<'EOF'
#!/bin/sh
for s in /etc/uci-defaults/9[0-9]-tollgate-*; do
    [ -x "$s" ] || continue
    "$s" && rm -f "$s"
done
/etc/init.d/tollgate enable 2>/dev/null || true
/etc/init.d/tollgate start  2>/dev/null || true
exit 0
EOF
cat > "$WORK/pre-deinstall" <<'EOF'
#!/bin/sh
/etc/init.d/tollgate stop    2>/dev/null || true
/etc/init.d/tollgate disable 2>/dev/null || true
exit 0
EOF

OUT="$DIST_DIR/${PKG_NAME}-${PKG_VERSION}.apk"

# fakeroot session: chown the staging tree to root, then mkpkg (which
# stats the tree inside the same session) records root:root ownership.
# apk mkpkg splits `depends` on spaces — commas are legal package-name
# characters and leak into the dependency names.
cat > "$WORK/mkpkg-inner.sh" <<EOF
#!/bin/sh
set -e
chown -R 0:0 "$STAGE"
exec "$APK" mkpkg -F "$STAGE" -o "$OUT" \\
    --info "name:$PKG_NAME" \\
    --info "version:$PKG_VERSION" \\
    --info "arch:$OPENWRT_ARCH" \\
    --info "description:TollGate - Cashu-metered network access" \\
    --info "license:MIT" \\
    --info "depends:nftables ip-full kmod-nft-tproxy" \\
    --script "post-install:$WORK/post-install" \\
    --script "pre-deinstall:$WORK/pre-deinstall"
EOF
fakeroot sh "$WORK/mkpkg-inner.sh"

echo "==> $OUT ($(du -h "$OUT" | cut -f1)) [arch:$OPENWRT_ARCH]"
echo "    Install: apk add --allow-untrusted $OUT"
