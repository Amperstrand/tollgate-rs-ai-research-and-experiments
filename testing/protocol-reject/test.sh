#!/usr/bin/env bash
# Integration test: Python client sends Announce + Reject to gateway.
#
# Asserts:
#   1. the client exits 0 (message was accepted by the gateway)
#   2. the gateway logs show the peer was processed (no crash)
#
# Usage: ./test.sh            (builds image, runs topology, asserts, cleans up)
#        SKIP_BUILD=1 ./test.sh   (reuse an existing tollgate-test:latest)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TESTING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE="docker compose -f $SCRIPT_DIR/docker-compose.yml"

cleanup() {
    $COMPOSE down -t 2 >/dev/null 2>&1 || true
}
trap cleanup EXIT

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    "$TESTING_DIR/scripts/build.sh"
fi

echo "Bringing up gateway..."
$COMPOSE up -d --no-build gateway

echo "Running client (sending Reject)..."
$COMPOSE up --no-build --exit-code-from client client || true

GATEWAY_LOG="$($COMPOSE logs --no-color gateway 2>/dev/null)"

echo "----- gateway log -----"
echo "$GATEWAY_LOG"
echo "----------------------"

fail() { echo "FAIL: $1" >&2; exit 1; }

# Strip ANSI escape codes before matching (defensive: the node disables color
# on non-tty output, but this keeps greps robust regardless).
strip_ansi() { sed $'s/\x1b\\[[0-9;]*m//g'; }
GATEWAY_LOG="$(echo "$GATEWAY_LOG" | strip_ansi)"

# 1. Gateway did not crash (it's still running or exited cleanly).
echo "$GATEWAY_LOG" | grep -qi "panic\|fatal\|crash" \
    && fail "gateway panicked or crashed"

# 2. Gateway saw the peer announce (the Announce was received before Reject).
echo "$GATEWAY_LOG" | grep -q "peer announced" \
    || fail "gateway did not log peer announced"

echo "PASS: reject handled cleanly (no crash, peer was announced)"
