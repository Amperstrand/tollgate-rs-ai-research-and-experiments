#!/bin/sh
# The .ipk produced by build-ipk.sh must be an opkg-readable ar container.
#
# An OpenWrt .ipk is an `ar` archive whose members are exactly:
#   debian-binary, control.tar.gz, data.tar.gz (in that order).
# opkg refuses tar-based containers ("Invalid package format").
#
# Run: sh packaging/openwrt-ipk/tests/test-build-ipk.sh

set -eu

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPT="$ROOT/packaging/openwrt-ipk/build-ipk.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v ar >/dev/null 2>&1 || fail "ar (binutils) required to run this test"

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

# 1. The container MUST be an ar archive (opkg cannot read tar containers).
ar t "$OUT" >/dev/null 2>&1 || \
    fail "container is not an ar archive — is build-ipk.sh still using tar?"

# 2. Exactly the three standard members, in order.
MEMBERS=$(ar t "$OUT" | tr '\n' ' ')
[ "$MEMBERS" = "debian-binary control.tar.gz data.tar.gz " ] \
    || fail "unexpected ar members: $MEMBERS"

# 3. debian-binary declares format 2.0.
ar p "$OUT" debian-binary > "$WORK/db"
[ "$(cat "$WORK/db")" = "2.0" ] || fail "debian-binary is not '2.0'"

# 4. data.tar.gz carries the tollgate binary.
ar p "$OUT" data.tar.gz > "$WORK/data.tar.gz"
tar -tzf "$WORK/data.tar.gz" ./usr/bin/tollgate >/dev/null 2>&1 || \
    fail "data.tar.gz missing ./usr/bin/tollgate"

echo "PASS: .ipk container is opkg-readable ar format"
