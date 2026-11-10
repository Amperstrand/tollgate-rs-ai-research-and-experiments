#!/bin/sh
# Test packaging/build-ipk.sh produces an opkg-readable .ipk.
#
# An OpenWrt .ipk is an `ar` archive whose members are exactly:
#   debian-binary, control.tar.gz, data.tar.gz (in that order).
# opkg refuses tar-based containers ("Invalid package format").
#
# Run: sh packaging/tests/test-build-ipk.sh

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$ROOT/packaging/build-ipk.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v ar >/dev/null 2>&1 || fail "ar (binutils) required to run this test"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Minimal payload tree.
mkdir -p "$WORK/payload/usr/bin"
printf '#!/bin/sh\necho tollgate-wrt\n' > "$WORK/payload/usr/bin/tollgate-wrt"

OUT="$WORK/tollgate-wrt_1.0_test.ipk"
env \
  PKG_NAME=tollgate-wrt \
  PKG_VERSION=1.0 \
  ARCH=test \
  sh "$SCRIPT" "$WORK/payload" "$OUT" >/dev/null

[ -f "$OUT" ] || fail "no output ipk produced"

# 1. The container MUST be an ar archive.
if ! ar t "$OUT" >/dev/null 2>&1; then
    fail "container is not an ar archive (opkg cannot read it) — is build-ipk.sh still using tar?"
fi

# 2. Exactly the three standard members, in order.
MEMBERS=$(ar t "$OUT" | tr '\n' ' ')
echo "members: $MEMBERS"
[ "$MEMBERS" = "debian-binary control.tar.gz data.tar.gz " ] \
    || fail "unexpected ar members: $MEMBERS"

# 3. debian-binary declares format 2.0.
ar p "$OUT" debian-binary > "$WORK/debian-binary"
[ "$(cat "$WORK/debian-binary")" = "2.0" ] || fail "debian-binary is not '2.0'"

# 4. control.tar.gz carries the control file with the package name.
ar p "$OUT" control.tar.gz > "$WORK/control.tar.gz"
tar -xzf "$WORK/control.tar.gz" -C "$WORK" ./control
grep -q '^Package: tollgate-wrt$' "$WORK/control" || fail "control file missing Package: tollgate-wrt"

# 5. data.tar.gz carries the payload file.
ar p "$OUT" data.tar.gz > "$WORK/data.tar.gz"
tar -xzf "$WORK/data.tar.gz" -C "$WORK" ./usr/bin/tollgate-wrt
grep -q 'tollgate-wrt' "$WORK/usr/bin/tollgate-wrt" || fail "payload file missing from data.tar.gz"

echo "PASS: build-ipk.sh emits an opkg-readable ar container"
