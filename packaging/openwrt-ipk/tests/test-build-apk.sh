#!/bin/sh
# build-apk.sh must produce an apk-tools v3 package (ADB magic) that
# installs on OpenWrt 25.12+ — the .ipk format is rejected there
# ("v2 package format error", verified 2026-11-15 on 25.12.6).
#
# Device-installability contract (verified against apk-tools 3.0.9):
#   - convenience arch aliases map to the OpenWrt arch (aarch64 ->
#     aarch64_cortex-a53); a package built with arch:aarch64 is
#     "error: uninstallable" on every real OpenWrt aarch64 device
#   - the staging tree is root:root (fakeroot); build-user ownership
#     installs as nobody:nogroup on the router
#   - depends match the .ipk: nftables, ip-full, kmod-nft-tproxy
#     (space-separated — apk mkpkg treats commas as name characters)
#   - post-install runs the uci-defaults and enables+starts the service
#   - a live /etc/tollgate/tollgate.yaml ships (not just the example)
#
# Requires: apk-tools 3 (set APK=... or have `apk` on PATH) and fakeroot.
# Run: APK=/path/to/apk sh packaging/openwrt-ipk/tests/test-build-apk.sh
set -eu

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPT="$ROOT/packaging/openwrt-ipk/build-apk.sh"
APK="${APK:-apk}"

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v "$APK" >/dev/null 2>&1 || { echo "SKIP: apk-tools 3 not available"; exit 0; }
command -v fakeroot >/dev/null 2>&1 || { echo "SKIP: fakeroot not available"; exit 0; }

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

# 3. Full metadata via adbdump for the device-installability contract.
"$APK" adbdump "$OUT" > "$WORK/dump.txt" 2>/dev/null || fail "adbdump failed"

# 3a. Arch alias mapping (x86_64 is its own OpenWrt arch; use the dump's
#     arch field to prove the mapping path, aarch64 is checked below).
grep -qx '  arch: x86_64' "$WORK/dump.txt" || fail "arch should be x86_64, got: $(grep '  arch:' "$WORK/dump.txt")"

# 3b. Dependencies: all three present, no comma artifacts.
for dep in nftables ip-full kmod-nft-tproxy; do
    grep -qx "    - $dep" "$WORK/dump.txt" || fail "missing dependency: $dep"
done
grep -q '    - .*,' "$WORK/dump.txt" && fail "comma artifact in depends list"

# 3c. root:root ownership — nothing owned by the build user.
if grep -q "user: $(id -un)" "$WORK/dump.txt"; then
    fail "package files owned by build user '$(id -un)' (fakeroot staging broken)"
fi
[ "$(grep -c 'user: root' "$WORK/dump.txt")" -gt 0 ] || fail "no root-owned files recorded"

# 3d. Live config ships next to the example.
grep -q 'name: tollgate.yaml$' "$WORK/dump.txt" || fail "no live /etc/tollgate/tollgate.yaml in package"

# 3e. post-install script attached.
grep -q 'post-install' "$WORK/dump.txt" || fail "no post-install script in package"

# 4. Arch alias mapping for aarch64 -> aarch64_cortex-a53 (the alias that
# produced uninstallable packages before the mapping existed).
PKG_VERSION=9.9.10-test APK="$APK" bash "$SCRIPT" --arch aarch64 --bin-dir "$WORK/bin" \
    > "$WORK/build2.log" 2>&1 || { cat "$WORK/build2.log" >&2; fail "build-apk.sh (aarch64) failed"; }
OUT2="$ROOT/dist/tollgate-9.9.10-r1.apk"
[ -f "$OUT2" ] || fail "expected $OUT2"
trap 'rm -rf "$WORK" "$OUT" "$OUT2"' EXIT
"$APK" adbdump "$OUT2" 2>/dev/null | grep -qx '  arch: aarch64_cortex-a53' \
    || fail "aarch64 alias must map to aarch64_cortex-a53"

echo "PASS: .apk package builds with v3 magic and parses"
