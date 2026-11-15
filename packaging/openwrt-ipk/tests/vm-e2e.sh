#!/usr/bin/env bash
# vm-e2e.sh — repeatable virtualized E2E for the OpenWrt package.
#
# LOCAL BENCH HARNESS, NOT CI: requires qemu-system-x86_64 (KVM strongly
# recommended — TCG boots take minutes), a local OpenWrt x86-64 image,
# and for .apk builds an apk-tools 3 binary. Skips gracefully when any
# requirement is absent; it exists to catch installer/payload/portal
# regressions before a bench flash, not to run on every push.
#
# Boots an OpenWrt x86-64 VM in QEMU (snapshot mode — the image is never
# mutated), installs the freshly built package, and verifies the payload:
# binary present + runs, portal served by uhttpd over HTTP. Exercises the
# SAME opkg/apk installer a router uses.
#
# Proven on 24.10.1 (ipk) and 25.12.6 (apk) — see tg-ai-deep-report.md
# (2026-11-15). This script is the repeatable form of that run.
#
# Requirements (skips gracefully when absent):
#   - qemu-system-x86_64
#   - an OpenWrt x86-64 image at $IMAGE (default: latest stable from the
#     local lab dir)
#   - apk-tools 3 on PATH or $APK (apk builds only)
#   - curl, ssh/scp (openssh), python3
#
# Usage:
#   sh packaging/openwrt-ipk/tests/vm-e2e.sh [--image PATH] [--format apk|ipk]
#
# Env: BIN_DIR (prebuilt tollgate+tolltop; default: build via cargo zigbuild)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
WORK="$(mktemp -d /tmp/tg-vm-e2e.XXXXXX)"
SSH_PORT="${TG_VM_E2E_SSH_PORT:-2231}"
HTTP_PORT="${TG_VM_E2E_HTTP_PORT:-8091}"
LAB_IMAGES="$HOME/tollgate-virtual-lab/images"
FORMAT="auto"

fail() { echo "FAIL: $1" >&2; exit 1; }
skip() { echo "SKIP: $1"; exit 0; }

cleanup() {
    local pid="$WORK/vm.pid"
    if [ -f "$pid" ]; then
        kill "$(cat "$pid")" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
    case "$1" in
        --image) IMAGE="$2"; shift 2 ;;
        --image=*) IMAGE="${1#*=}"; shift ;;
        --format) FORMAT="$2"; shift 2 ;;
        --format=*) FORMAT="${1#*=}"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

command -v qemu-system-x86_64 >/dev/null 2>&1 || skip "qemu-system-x86_64 not installed"
command -v curl >/dev/null 2>&1 || fail "curl required"
command -v ssh >/dev/null 2>&1 || fail "openssh client required"

# Pick the latest lab image when not given one (25.x only — the primary
# target; pass --image explicitly for legacy 24.10 runs).
if [ -z "${IMAGE:-}" ]; then
    for cand in $(ls -1 "$LAB_IMAGES"/openwrt-25.*-x86-64.img 2>/dev/null | sort -V | tail -1); do
        IMAGE="$cand"
    done
fi
[ -n "${IMAGE:-}" ] || skip "no OpenWrt 25.x x86-64 image found (set --image)"
echo "==> image: $IMAGE"

# Determine target format from the image version.
if [ "$FORMAT" = "auto" ]; then
    case "$IMAGE" in
        *25.1[2-9]*|*2[6-9].*) FORMAT=apk ;;
        *) FORMAT=ipk ;;
    esac
fi
echo "==> format: $FORMAT"

# Binaries: prebuilt or cargo zigbuild (x86_64 musl, static).
BIN_DIR="${BIN_DIR:-$WORK/bin}"
if [ ! -f "$BIN_DIR/tollgate" ]; then
    command -v cargo-zigbuild >/dev/null 2>&1 \
        || skip "no prebuilt binaries and cargo-zigbuild absent (set BIN_DIR)"
    echo "==> building static musl binaries (x86_64)..."
    (cd "$PROJECT_ROOT" && CARGO_TARGET_DIR="$WORK/target" \
        cargo zigbuild --release --target x86_64-unknown-linux-musl \
            --features v1-compat --bin tollgate --bin tolltop -p tollgate-net \
        >/dev/null)
    mkdir -p "$BIN_DIR"
    cp "$WORK/target/x86_64-unknown-linux-musl/release/tollgate" \
       "$WORK/target/x86_64-unknown-linux-musl/release/tolltop" "$BIN_DIR/"
fi

# Build the package.
echo "==> building .$FORMAT package..."
case "$FORMAT" in
    ipk)
        PKG_VERSION=99.0.0-e2e bash "$SCRIPT_DIR/../build-ipk.sh" \
            --arch x86_64 --bin-dir "$BIN_DIR" >/dev/null
        PKG_FILE="$PROJECT_ROOT/dist/tollgate_99.0.0-e2e_x86_64.ipk"
        INSTALL_CMD="opkg install /tmp/pkg.ipk"
        NET_SETUP="opkg update"
        ;;
    apk)
        command -v "${APK:-apk}" >/dev/null 2>&1 \
            || skip "apk-tools 3 not available for .apk builds (set APK=)"
        PKG_VERSION=99.0.0-e2e APK="${APK:-apk}" bash "$SCRIPT_DIR/../build-apk.sh" \
            --arch x86_64 --bin-dir "$BIN_DIR" >/dev/null
        PKG_FILE="$PROJECT_ROOT/dist/tollgate-99.0.0-r2.apk"
        INSTALL_CMD="apk add --allow-untrusted /tmp/pkg.apk"
        NET_SETUP="true"  # apk resolves deps from the configured repos
        ;;
esac
[ -f "$PKG_FILE" ] || fail "package build produced nothing"
echo "    $(du -h "$PKG_FILE" | cut -f1) $PKG_FILE"

# Boot the VM (snapshot mode; slirp forwards to the guest's static
# 192.168.1.1 — plus the return route the guest needs, set over serial).
echo "==> booting QEMU (ssh :$SSH_PORT, http :$HTTP_PORT)..."
qemu-system-x86_64 -machine pc -cpu max -m 512M \
    -drive file="$IMAGE",format=raw,if=virtio,snapshot=on \
    -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$SSH_PORT-192.168.1.1:22,hostfwd=tcp:127.0.0.1:$HTTP_PORT-192.168.1.1:80" \
    -device virtio-net-pci,netdev=net0 \
    -display none -serial pty -monitor none -daemonize \
    -pidfile "$WORK/vm.pid" >"$WORK/vm.out" 2>"$WORK/vm.err"

# The guest's static LAN config has no route back to QEMU's slirp network
# (10.0.2.x) — without it, hostfwd TCP handshakes stall forever. Stage the
# routes over the serial console BEFORE waiting for ssh (boot + first-boot
# rootfs expansion can take a couple of minutes under TCG).
# qemu prints the pty redirect notice on STDOUT when daemonizing.
PTY="$(grep -ho '/dev/pts/[0-9]*' "$WORK/vm.out" "$WORK/vm.err" 2>/dev/null | head -1)"
[ -n "$PTY" ] || fail "qemu did not report a serial pty"
python3 - "$PTY" <<'PYDRV'
import os, select, sys, time
fd = os.open(sys.argv[1], os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
def drain(t):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.3)
        if r:
            try: os.read(fd, 4096)
            except BlockingIOError: pass
def send(line):
    os.write(fd, (line + "\n").encode())
deadline = time.time() + 240
prompt = False
while time.time() < deadline and not prompt:
    r, _, _ = select.select([fd], [], [], 1.0)
    if r:
        try:
            data = os.read(fd, 4096)
            if b"Please press Enter" in data or b"root@" in data:
                prompt = True
        except BlockingIOError: pass
send("")          # activate the console
drain(2)
send("ip route add 10.0.2.2/32 dev br-lan 2>/dev/null; "
     "ip route add default via 10.0.2.2 dev br-lan 2>/dev/null; "
     "echo nameserver 10.0.2.3 > /etc/resolv.conf; echo ROUTES-STAGED")
drain(3)
os.close(fd)
PYDRV
echo "==> serial: return routes staged"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -p "$SSH_PORT")

# Wait for SSH (first boot does rootfs expansion; give it 3 minutes).
echo "==> waiting for ssh..."
ready=""
for _ in $(seq 1 60); do
    if ssh "${SSH_OPTS[@]}" root@127.0.0.1 true >/dev/null 2>&1; then ready=1; break; fi
    sleep 5
done
[ -n "$ready" ] || { cat "$WORK/vm.err" >&2; fail "VM never became ssh-reachable"; }
sleep 5  # let dropbear settle before the first payload transfer

# The guest has no default route to slirp (static LAN config); add one so
# dependency fetch + install works. ipk needs opkg update first.
ssh "${SSH_OPTS[@]}" root@127.0.0.1 \
    "ip route add 10.0.2.2/32 dev br-lan 2>/dev/null; \
     ip route add default via 10.0.2.2 dev br-lan 2>/dev/null; \
     echo 'nameserver 10.0.2.3' > /etc/resolv.conf; \
     $NET_SETUP" >/dev/null 2>&1 || true

# Transfer + install (retry: dropbear occasionally drops the first
# connection right after boot).
# Transfer via ssh stdin (scp's server binary is not guaranteed on
# minimal images; `cat >` needs only the shell).
remote_pkg=$([ "$FORMAT" = ipk ] && echo /tmp/pkg.ipk || echo /tmp/pkg.apk)
xfer=0
for attempt in 1 2 3; do
    if ssh "${SSH_OPTS[@]}" root@127.0.0.1 "cat > $remote_pkg" < "$PKG_FILE"         >/dev/null 2>&1; then xfer=1; fi
    [ "$xfer" = 1 ] && break
    echo "    transfer attempt $attempt failed; retrying..."
    sleep 5
done
[ "$xfer" = 1 ] || fail "package transfer failed after 3 attempts"
# Verify the transfer landed intact (size match).
local_size="$(wc -c < "$PKG_FILE" | tr -d ' ')"
remote_size="$(ssh "${SSH_OPTS[@]}" root@127.0.0.1 "wc -c < $remote_pkg" 2>/dev/null | tr -d ' ')"
[ "$local_size" = "$remote_size" ] || fail "transfer corrupted: $local_size vs $remote_size"
echo "==> installing ($FORMAT)..."
ssh "${SSH_OPTS[@]}" root@127.0.0.1 "$INSTALL_CMD" >"$WORK/install.log" 2>&1 \
    || { cat "$WORK/install.log" >&2; fail "$FORMAT install failed"; }

# Verify: binary, portal, uci-defaults.
ssh "${SSH_OPTS[@]}" root@127.0.0.1 \
    "test -x /usr/bin/tollgate || exit 10; \
     /usr/bin/tollgate --help >/dev/null 2>&1 || exit 11; \
     test -f /www/tollgate-captive-portal-site/manifest.json || exit 12" \
    >/dev/null 2>&1 \
    || fail "payload incomplete (binary/portal missing)"

for path in manifest.json splash.html; do
    code="$(curl -s -o /dev/null -w '%{http_code}' \
        "http://127.0.0.1:$HTTP_PORT/tollgate-captive-portal-site/$path")"
    [ "$code" = "200" ] || fail "uhttpd portal $path -> $code"
done

echo "PASS: .$FORMAT installs on OpenWrt ($(basename "$IMAGE")); binary runs; portal served"
