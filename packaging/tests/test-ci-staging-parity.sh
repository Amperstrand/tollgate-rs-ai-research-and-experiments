#!/bin/sh
# CI IPK staging must include every first-boot uci-defaults script.
#
# build-package.yml stages the payload inline (duplicating
# scripts/build-ipk-local.sh). Any packaging/files/etc/uci-defaults/* file
# missing from the workflow's staging step means first-boot parity is broken:
# locally built IPKs and CI IPKs behave differently after flash.
#
# Run: sh packaging/tests/test-ci-staging-parity.sh

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/build-package.yml"

fail() { echo "FAIL: $1" >&2; exit 1; }

[ -f "$WORKFLOW" ] || fail "workflow not found: $WORKFLOW"

for script_path in "$ROOT"/packaging/files/etc/uci-defaults/*; do
    [ -f "$script_path" ] || fail "no uci-defaults scripts found under packaging/files"
    script="$(basename "$script_path")"
    grep -q "uci-defaults/$script" "$WORKFLOW" \
        || fail "CI does not stage uci-defaults/$script — first-boot parity broken vs local build"
    echo "staged: $script"
done

echo "PASS: CI stages every uci-defaults first-boot script"
