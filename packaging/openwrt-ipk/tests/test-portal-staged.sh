#!/bin/sh
# The vendored captive portal must ship in the .ipk under /www (uhttpd
# serves it with zero wiring) — and the build must not fetch it from any
# upstream repo at package time (read-only-upstream rule; offline builds).
set -eu
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPT="$ROOT/packaging/openwrt-ipk/build-ipk.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v ar >/dev/null 2>&1 || fail "ar (binutils) required"
command -v tar >/dev/null 2>&1 || fail "tar required"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin"
printf '#!/bin/sh\necho tollgate\n' > "$WORK/bin/tollgate"
printf '#!/bin/sh\necho tolltop\n'  > "$WORK/bin/tolltop"
chmod +x "$WORK/bin/tollgate" "$WORK/bin/tolltop"

PKG_VERSION=1.0.0-test bash "$SCRIPT" --arch x86_64 --bin-dir "$WORK/bin" \
    > "$WORK/build.log" 2>&1 || { cat "$WORK/build.log" >&2; fail "build failed"; }

OUT="$ROOT/dist/tollgate_1.0.0-test_x86_64.ipk"
[ -f "$OUT" ] || fail "no ipk produced"
trap 'rm -rf "$WORK" "$OUT"' EXIT

mkdir -p "$WORK/unpack"
(cd "$WORK/unpack" && tar -xzf "$OUT" ./data.tar.gz && tar -xzf data.tar.gz)

[ -f "$WORK/unpack/www/tollgate-captive-portal-site/manifest.json" ] \
    || fail "portal manifest.json missing from /www in the ipk"
[ -d "$WORK/unpack/www/tollgate-captive-portal-site/locales" ] \
    || fail "portal locales/ missing from the ipk"

if grep -rEn --exclude="test-portal-staged.sh" "api.github.com|raw.githubusercontent.com" "$ROOT/packaging" >/dev/null 2>&1; then
    fail "packaging still references upstream repos at build time"
fi

echo "PASS: portal vendored into /www in the ipk; no upstream fetches at build time"
