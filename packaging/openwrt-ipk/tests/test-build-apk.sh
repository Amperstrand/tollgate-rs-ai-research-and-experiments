#!/bin/sh
# build-apk.sh must produce an apk-tools v3 package (ADB magic) that
# installs on OpenWrt 25.12+ — the .ipk format is rejected there
# ("v2 package format error", verified 2026-11-15 on 25.12.6).
#
# Requires: apk-tools 3 (set APK=... or have `apk` on PATH).
# Run: APK=/path/to/apk sh packaging/openwrt-ipk/tests/test-build-apk.sh
set -eu

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPT="$ROOT/packaging/openwrt-ipk/build-apk.sh"
APK="${APK:-apk}"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v "$APK" >/dev/null 2>&1 || { echo "SKIP: apk-tools 3 not available"; exit 0; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin"
printf '#!/bin/sh\necho tollgate\n' > "$WORK/bin/tollgate"
printf '#!/bin/sh\necho tolltop\n'  > "$WORK/bin/tolltop"
chmod +x "$WORK/bin/tollgate" "$WORK/bin/tolltop"

PKG_VERSION=9.9.9-test APK="$APK" bash "$SCRIPT" --arch x86_64 --bin-dir "$WORK/bin" \
    > "$WORK/build.log" 2>&1 || { cat "$WORK/build.log" >&2; fail "build-apk.sh failed"; }

OUT="$ROOT/dist/tollgate-9.9.9-r1.apk"
[ -f "$OUT" ] || fail "expected $OUT"
trap 'rm -rf "$WORK" "$OUT"' EXIT

# 1. ADB magic (apk-tools v3 container).
python3 -c "import sys; d=open('$OUT','rb').read(4); sys.exit(0 if d==b'ADBd' else 1)" \
    || fail "not an apk-tools v3 package (magic $(python3 -c "print(open('$OUT','rb').read(4))"))"

# 2. apk can read its own metadata back.
"$APK" info --allow-untrusted --print-arch "$OUT" >/dev/null 2>&1 \
    || "$APK" manifest "$OUT" >/dev/null 2>&1 \
    || fail "apk cannot parse the package"

echo "PASS: .apk package builds with v3 magic and parses"
