#!/bin/sh
# The .ipk produced by build-ipk.sh must carry the three standard members
# in an opkg-readable outer container.
#
# Upstream OpenWrt's scripts/ipkg-build emits a gzip tar outer container
# (./debian-binary ./control.tar.gz ./data.tar.gz); opkg accepts both
# tar.gz and ar outers. This pins the script's tar.gz contract — member
# presence and names, not just "a file was produced".
#
# Run: sh packaging/openwrt-ipk/tests/test-build-ipk.sh

set -eu

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPT="$ROOT/packaging/openwrt-ipk/build-ipk.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Stub prebuilt binaries so the script runs without a toolchain.
mkdir -p "$WORK/bin"
printf '#!/bin/sh\necho tollgate\n' > "$WORK/bin/tollgate"
printf '#!/bin/sh\necho tolltop\n'  > "$WORK/bin/tolltop"
chmod +x "$WORK/bin/tollgate" "$WORK/bin/tolltop"

PKG_VERSION=1.0.0-test bash "$SCRIPT" --arch x86_64 --bin-dir "$WORK/bin" \
    > "$WORK/build.log" 2>&1 || { cat "$WORK/build.log" >&2; fail "build-ipk.sh failed"; }

OUT="$ROOT/dist/tollgate_1.0.0-test_x86_64.ipk"
[ -f "$OUT" ] || fail "no output ipk at $OUT"
trap 'rm -rf "$WORK" "$OUT"' EXIT

# 1. The outer container decompresses as the tar.gz OpenWrt-native shape.
MEMBERS=$(tar -tzf "$OUT" | tr '\n' ' ')
echo "members: $MEMBERS"
for member in ./debian-binary ./control.tar.gz ./data.tar.gz; do
    case "$MEMBERS" in
        *"$member"*) ;;
        *) fail "outer container missing $member" ;;
    esac
done

# 2. debian-binary declares format 2.0.
tar -xzf "$OUT" -C "$WORK" ./debian-binary
[ "$(cat "$WORK/debian-binary")" = "2.0" ] || fail "debian-binary is not '2.0'"

# 3. data.tar.gz carries the tollgate binary.
tar -xzf "$OUT" -C "$WORK" ./data.tar.gz
tar -tzf "$WORK/data.tar.gz" ./usr/bin/tollgate >/dev/null 2>&1 || \
    fail "data.tar.gz missing ./usr/bin/tollgate"

echo "PASS: .ipk outer container carries the three standard members (tar.gz, upstream-native)"
