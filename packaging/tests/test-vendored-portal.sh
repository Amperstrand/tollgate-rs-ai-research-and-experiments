#!/bin/sh
# Vendored portal assets must be present, and packaging must not reference
# the OpenTollGate upstream repos (read-only-upstream rule; offline builds).
set -eu
cd "$(dirname "$0")/../.."

PORTAL=packaging/files/tollgate-captive-portal-site

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

test -f "$PORTAL/manifest.json" || fail "vendored portal manifest.json missing"
test -f "$PORTAL/splash.html" || fail "vendored portal splash.html missing"
test -f "$PORTAL/favicon.ico" || fail "vendored portal favicon.ico missing"
test -d "$PORTAL/locales" || fail "vendored portal locales/ missing"

if grep -rn "api.github.com/repos/OpenTollGate" .github/workflows packaging/build-ipk.sh 2>/dev/null; then
    fail "packaging still queries the OpenTollGate GitHub API"
fi
if grep -rn "raw.githubusercontent.com/OpenTollGate" .github/workflows packaging/build-ipk.sh 2>/dev/null; then
    fail "packaging still downloads from raw.githubusercontent OpenTollGate"
fi

echo "PASS: portal assets vendored ($PORTAL), no upstream references in packaging"
