#!/usr/bin/env python3
"""Two-node TollGate v2 protocol test on SHC VMs.

Orders a gateway VM and a client VM from SHC, deploys TollGate on each, starts
a fake Cashu mint on the gateway, and exercises the full v2 payment lifecycle:

  1. Gateway runs ``tollgate serve`` (sells metered access).
  2. Gateway runs ``fake-mint.py`` (NUT-07 check-state stub on :3338).
  3. Client runs ``tollgate pay``    — bootstrap token to the gateway.
  4. Client runs ``tollgate consume`` — auto-topup loop (limited polls).
  5. Gateway runs ``tolltop --once``  — verify peer Active with balance > 0.

Assertions:
  - ``pay`` output contains ``accepted=true``
  - ``tolltop --once`` shows the peer state ``Active``
  - ``tolltop --once`` shows ``WE_HOLD`` balance > 0

Both VMs are cancelled in a ``finally`` block.
"""
import os
import sys
import time
import subprocess
import base64
import re

sys.path.insert(0, os.environ.get("SHC_TOOLKIT_PATH", "/home/ubuntu/src/shc-toolkit"))
from shc_toolkit.client import SHCClient  # noqa: E402

# ── Paths ────────────────────────────────────────────────────────────────
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(REPO, "target", "release", "tollgate")
TOLLTOP = os.path.join(REPO, "target", "release", "tolltop")
FAKE_MINT = os.path.join(REPO, "testing", "bootstrap", "fake-mint.py")
SSH_PUB = os.path.expanduser("~/.ssh/tollgate_cloud_key.pub")
SSH_PRIV = os.path.expanduser("~/.ssh/tollgate_cloud_key")

SSH_BASE = [
    "ssh",
    "-o", "StrictHostKeyChecking=no",
    "-o", "UserKnownHostsFile=/dev/null",
    "-o", "ConnectTimeout=10",
    "-o", "LogLevel=ERROR",
    "-i", SSH_PRIV,
]
SCP_BASE = [
    "scp",
    "-o", "StrictHostKeyChecking=no",
    "-o", "UserKnownHostsFile=/dev/null",
    "-o", "LogLevel=ERROR",
    "-i", SSH_PRIV,
]

# ── Network constants ────────────────────────────────────────────────────
GW_PORT = 4747        # gateway tollgate listen port
MINT_PORT = 3338      # fake-mint port
PAY_AMOUNT = 20       # sats per token
CONSUME_POLLS = 5     # number of consume poll cycles


# ── Helpers ──────────────────────────────────────────────────────────────

def ssh(ip, *cmd, timeout=30):
    """Run a command on *ip* as ``debian``.  Returns ``CompletedProcess``."""
    return subprocess.run(
        SSH_BASE + [f"debian@{ip}"] + list(cmd),
        capture_output=True,
        timeout=timeout,
    )


def ssh_shell(ip, command, timeout=30):
    """Run a shell command string on *ip* (for redirections / backgrounding)."""
    return subprocess.run(
        SSH_BASE + [f"debian@{ip}", command],
        capture_output=True,
        timeout=timeout,
    )


def scp(local, ip, remote, timeout=180):
    """Copy *local* to *ip:remote*."""
    return subprocess.run(
        SCP_BASE + [local, f"debian@{ip}:{remote}"],
        capture_output=True,
        timeout=timeout,
    )


def get_ip(detail):
    """Extract the primary IPv4 address from a VM detail dict."""
    if isinstance(detail.get("ips"), list) and detail["ips"]:
        return detail["ips"][0].get("ip")
    return detail.get("ip_address") or detail.get("ip")


def wait_for_vm(client, service_id, label):
    """Poll until the VM is provisioned and SSH-accessible.

    Returns ``(ip, user)`` or ``(None, None)`` on failure.
    """
    print(f"\n[{label}] Waiting for provisioning (service {service_id})...")
    time.sleep(15)
    ip = None
    for attempt in range(40):
        try:
            detail = client.get_vm_detail(service_id)
            ip = get_ip(detail)
            state = detail.get("provisioning_state", "?")
            if ip and state in ("ready", "active"):
                print(f"  [{label}] Provisioned: {ip} (state={state})")
                break
            print(f"  [{label}] [{attempt + 1}/40] state={state} ip={ip or '?'}")
        except Exception as exc:
            print(f"  [{label}] [{attempt + 1}/40] {str(exc)[:80]}")
        time.sleep(10)

    if not ip:
        return None, None

    # Push SSH key live (idempotent — ignore errors on already-keyed VMs).
    try:
        with open(SSH_PUB) as fh:
            client.apply_ssh_key_live(service_id, fh.read().strip())
    except Exception:
        pass
    time.sleep(5)

    # Wait for SSH to become available.
    print(f"  [{label}] Waiting for SSH...")
    for _ in range(15):
        for user in ("debian", "root"):
            r = ssh(ip, "echo", "OK", timeout=10)
            if r.returncode == 0 and b"OK" in r.stdout:
                print(f"  [{label}] SSH OK: {user}@{ip}")
                return ip, user
        time.sleep(10)

    print(f"  [{label}] SSH never became available")
    return None, None


def deploy_binary(ip, local_path, remote_path, label=""):
    """SCP a binary to the VM and make it executable."""
    size_mb = os.path.getsize(local_path) // 1_048_576
    name = os.path.basename(remote_path)
    print(f"  [{label}] Deploying {name} ({size_mb} MB)...")
    scp(local_path, ip, "/tmp/_deploy_bin")
    ssh_shell(ip, f"sudo mv /tmp/_deploy_bin {remote_path} && sudo chmod +x {remote_path}")


def write_remote_config(ip, config_yaml, remote_path="/etc/tollgate.yaml"):
    """Write *config_yaml* to *remote_path* on the VM via base64 piping."""
    b64 = base64.b64encode(config_yaml.encode()).decode()
    ssh_shell(ip, f"echo '{b64}' | base64 -d | sudo tee {remote_path} > /dev/null")


def wait_for_http(url, attempts=30, interval=2):
    """Poll *url* until it returns HTTP 200.  Returns True/False."""
    for _ in range(attempts):
        time.sleep(interval)
        r = subprocess.run(
            ["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", url],
            capture_output=True,
            timeout=5,
        )
        if r.stdout.decode().strip() == "200":
            return True
    return False


def parse_we_hold(tolltop_output):
    """Extract the WE_HOLD value from the tolltop --once Active peer line.

    The tolltop output is a fixed-width text table.  Column positions
    (0-indexed, from ``render_table`` in status.rs):

      PEER(0-12) IP(14-32) STATE(34-43) DELIVERED(45-55) RECEIVED(57-67)
      WE_HOLD(69-76) THEY_HOLD(78-86) NET(88-93) DRIFT(95-100) METERED

    Returns the integer balance (>0 when the peer has prepaid credit), or
    ``None`` if no Active peer is found.
    """
    for line in tolltop_output.splitlines():
        stripped = line.strip()
        # Skip header and summary lines.
        if not stripped or stripped.startswith("PEER") or stripped.startswith("node ") \
           or "peers (" in stripped or "pricing" in stripped.lower():
            continue
        # WE_HOLD column is characters 69:77.
        if len(line) >= 77:
            we_hold_raw = line[69:77].strip()
        else:
            # Fallback: use regex to find a standalone number/dash after Active.
            m = re.search(r"Active\s+.*?\s+(\S+)\s+\S+\s+\S+\s+\S+s", line)
            we_hold_raw = m.group(1) if m else "-"
        if we_hold_raw and we_hold_raw != "-":
            try:
                return int(we_hold_raw)
            except ValueError:
                pass
    return None


# ── Main ─────────────────────────────────────────────────────────────────

def main():
    if not os.path.isfile(BINARY):
        print(f"FATAL: tollgate binary not found at {BINARY}")
        sys.exit(1)
    if not os.path.isfile(TOLLTOP):
        print(f"FATAL: tolltop binary not found at {TOLLTOP}")
        sys.exit(1)
    if not os.path.isfile(FAKE_MINT):
        print(f"FATAL: fake-mint.py not found at {FAKE_MINT}")
        sys.exit(1)

    with open(SSH_PUB) as fh:
        pubkey = fh.read().strip()

    client = SHCClient()
    balance = client.get_account_balance()
    credit = float(balance["credit"][0]["amount"])
    print(f"Account balance: ${credit:.2f}")

    ts = int(time.time())
    gw_sid = None
    cl_sid = None

    # ── Order both VMs ───────────────────────────────────────────────
    print("\n" + "=" * 60)
    print("  Ordering SHC VMs")
    print("=" * 60)

    gw_vm = client.order_vm(
        hostname=f"tg-gw-{ts}", package_id=81, pricing_id=245, ssh_key=pubkey,
    )
    gw_sid = gw_vm.get("service_id") or gw_vm.get("id")
    print(f"  Gateway ordered: service_id={gw_sid}")

    cl_vm = client.order_vm(
        hostname=f"tg-cl-{ts}", package_id=81, pricing_id=245, ssh_key=pubkey,
    )
    cl_sid = cl_vm.get("service_id") or cl_vm.get("id")
    print(f"  Client  ordered: service_id={cl_sid}")

    try:
        # ── Wait for provisioning + SSH (PARALLEL via threads) ──────
        import threading

        results = {}
        def provision(label, sid):
            ip, user = wait_for_vm(client, sid, label)
            results[label] = (ip, user)

        t_gw = threading.Thread(target=provision, args=("gateway", gw_sid))
        t_cl = threading.Thread(target=provision, args=("client", cl_sid))
        t_gw.start()
        t_cl.start()
        t_gw.join(timeout=400)
        t_cl.join(timeout=400)

        gw_ip = results.get("gateway", (None, None))[0]
        cl_ip = results.get("client", (None, None))[0]

        if not gw_ip:
            print("FATAL: Gateway VM unreachable — aborting.")
            return
        if not cl_ip:
            print("FATAL: Client VM unreachable — aborting.")
            return

        print(f"\nGateway IP: {gw_ip}")
        print(f"Client  IP: {cl_ip}")

        # ── Deploy fake-mint to gateway ─────────────────────────────
        print("\n" + "-" * 60)
        print("  Deploying fake-mint to gateway")
        print("-" * 60)
        scp(FAKE_MINT, gw_ip, "/tmp/fake-mint.py")
        ssh_shell(gw_ip, "nohup python3 /tmp/fake-mint.py "
                         f"{MINT_PORT} > /tmp/fake-mint.log 2>&1 &")
        time.sleep(2)

        # Verify fake-mint is responding.
        r = ssh(gw_ip, "curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                f"http://127.0.0.1:{MINT_PORT}/v1/info")
        fm_code = r.stdout.decode().strip()
        if fm_code != "200":
            print(f"  WARNING: fake-mint returned HTTP {fm_code}, checking log:")
            ssh(gw_ip, "cat", "/tmp/fake-mint.log")
        else:
            print(f"  fake-mint is UP on :{MINT_PORT}")

        print("  Checking port connectivity client→gateway...")
        r = ssh(cl_ip, "curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                f"http://{gw_ip}:{MINT_PORT}/v1/info", timeout=10)
        cross_code = r.stdout.decode().strip()
        if cross_code != "200":
            print(f"  WARNING: Client cannot reach gateway:{MINT_PORT} (HTTP {cross_code})")
            print("  fake-mint may need to bind to 0.0.0.0 or firewall needs opening")
            ssh_shell(gw_ip, f"sudo iptables -I INPUT -p tcp --dport {MINT_PORT} -j ACCEPT 2>/dev/null; sudo iptables -I INPUT -p tcp --dport {GW_PORT} -j ACCEPT 2>/dev/null; true")
            time.sleep(1)
            r = ssh(cl_ip, "curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                    f"http://{gw_ip}:{MINT_PORT}/v1/info", timeout=10)
            cross_code = r.stdout.decode().strip()
            print(f"  After firewall fix: HTTP {cross_code}")
        else:
            print(f"  Client→gateway:{MINT_PORT} OK")

        # ── Deploy tollgate + tolltop to gateway ────────────────────
        print("\n" + "-" * 60)
        print("  Deploying tollgate + tolltop to gateway")
        print("-" * 60)
        deploy_binary(gw_ip, BINARY, "/usr/local/bin/tollgate", "gw")
        deploy_binary(gw_ip, TOLLTOP, "/usr/local/bin/tolltop", "gw")

        # ── Deploy tollgate to client ───────────────────────────────
        print("\n" + "-" * 60)
        print("  Deploying tollgate to client")
        print("-" * 60)
        deploy_binary(cl_ip, BINARY, "/usr/local/bin/tollgate", "cl")

        # ── Configure + start gateway server ───────────────────────
        print("\n" + "-" * 60)
        print("  Configuring gateway")
        print("-" * 60)
        gw_config = (
            f'listen: "0.0.0.0:{GW_PORT}"\n'
            'unit: "bytes"\n'
            'mints:\n'
            f'  - "http://127.0.0.1:{MINT_PORT}"\n'
            f'  - "http://{gw_ip}:{MINT_PORT}"\n'
            'firewall: sets-only\n'
            'metering_interval_secs: 5\n'
            'products:\n'
            '  - pricing_scale: 1000\n'
            '    price_per_second: 0\n'
            '    price_per_unit: 1\n'
        )
        write_remote_config(gw_ip, gw_config)
        print("  Gateway config written to /etc/tollgate.yaml")

        print("  Starting gateway server...")
        ssh_shell(
            gw_ip,
            f"nohup /usr/local/bin/tollgate --config /etc/tollgate.yaml "
            f"serve --listen 0.0.0.0:{GW_PORT} > /tmp/tollgate.log 2>&1 &",
        )

        print("  Waiting for gateway to accept connections...")
        if not wait_for_http(f"http://{gw_ip}:{GW_PORT}/"):
            print("  FATAL: Gateway server did not come up. Log tail:")
            ssh(gw_ip, "tail", "-30", "/tmp/tollgate.log")
            return
        print("  Gateway server is UP!")

        # ── Configure client ────────────────────────────────────────
        print("\n" + "-" * 60)
        print("  Configuring client")
        print("-" * 60)
        cl_config = (
            'listen: "127.0.0.1:4748"\n'
            'unit: "bytes"\n'
            'mints:\n'
            f'  - "http://{gw_ip}:{MINT_PORT}"\n'
            'firewall: sets-only\n'
            'metering_interval_secs: 5\n'
        )
        write_remote_config(cl_ip, cl_config)
        print("  Client config written to /etc/tollgate.yaml")

        # ── Run ``tollgate pay`` from client ────────────────────────
        print("\n" + "=" * 60)
        print("  STEP 1: tollgate pay  (client → gateway)")
        print("=" * 60)
        peer_url = f"http://{gw_ip}:{GW_PORT}"
        mint_url = f"http://{gw_ip}:{MINT_PORT}"

        r = ssh(
            cl_ip,
            "/usr/local/bin/tollgate", "--config", "/etc/tollgate.yaml",
            "pay",
            "--peer", peer_url,
            "--mint", mint_url,
            "--amount", str(PAY_AMOUNT),
            timeout=60,
        )
        pay_stdout = r.stdout.decode()
        pay_stderr = r.stderr.decode()
        pay_output = pay_stdout + "\n" + pay_stderr
        print(f"  exit code: {r.returncode}")
        if pay_stdout.strip():
            print(f"  stdout:\n{pay_stdout.rstrip()}")
        if pay_stderr.strip():
            print(f"  stderr:\n{pay_stderr.rstrip()}")

        # ── Run ``tollgate consume`` from client ────────────────────
        print("\n" + "=" * 60)
        print(f"  STEP 2: tollgate consume  ({CONSUME_POLLS} polls)")
        print("=" * 60)
        r = ssh(
            cl_ip,
            "/usr/local/bin/tollgate", "--config", "/etc/tollgate.yaml",
            "consume",
            "--peer", peer_url,
            "--mint", mint_url,
            "--amount", str(PAY_AMOUNT),
            "--topup", str(PAY_AMOUNT),
            "--polls", str(CONSUME_POLLS),
            timeout=120,
        )
        consume_stdout = r.stdout.decode()
        consume_stderr = r.stderr.decode()
        consume_output = consume_stdout + "\n" + consume_stderr
        print(f"  exit code: {r.returncode}")
        if consume_stdout.strip():
            print(f"  stdout:\n{consume_stdout.rstrip()}")
        if consume_stderr.strip():
            print(f"  stderr:\n{consume_stderr.rstrip()}")

        # ── Check ``tolltop --once`` on gateway ─────────────────────
        print("\n" + "=" * 60)
        print("  STEP 3: tolltop --once  (on gateway)")
        print("=" * 60)
        time.sleep(3)  # Let the gateway settle after the consume loop.
        r = ssh(gw_ip, "/usr/local/bin/tolltop", "--once", timeout=15)
        tolltop_output = r.stdout.decode()
        tolltop_err = r.stderr.decode()
        print(f"  exit code: {r.returncode}")
        if tolltop_output.strip():
            print(f"  output:\n{tolltop_output.rstrip()}")
        if tolltop_err.strip():
            print(f"  stderr:\n{tolltop_err.rstrip()}")

        # ── Assertions ──────────────────────────────────────────────
        print("\n" + "=" * 60)
        print("  TEST RESULTS")
        print("=" * 60)

        results = []

        # Test 1: pay output contains accepted=true
        t1 = "accepted=true" in pay_output
        results.append(t1)
        status = "PASS" if t1 else "FAIL"
        print(f"  [{status}] pay output contains 'accepted=true'")

        # Test 2: tolltop shows at least one Active peer
        # Look for "Active" in a data line (not header or summary).
        has_active = False
        for line in tolltop_output.splitlines():
            s = line.strip()
            if s and not s.startswith("PEER") and not s.startswith("node ") \
               and "peers (" not in s and "pricing" not in s.lower():
                if "Active" in line:
                    has_active = True
                    break
        results.append(has_active)
        status = "PASS" if has_active else "FAIL"
        print(f"  [{status}] tolltop shows peer state Active")

        # Test 3: tolltop shows WE_HOLD balance > 0
        we_hold = parse_we_hold(tolltop_output)
        t3 = we_hold is not None and we_hold > 0
        results.append(t3)
        status = "PASS" if t3 else "FAIL"
        detail = f"WE_HOLD={we_hold}" if we_hold is not None else "WE_HOLD not found"
        print(f"  [{status}] tolltop shows WE_HOLD balance > 0  ({detail})")

        # ── Summary ─────────────────────────────────────────────────
        passed = sum(1 for r in results if r)
        total = len(results)
        print(f"\n  {passed}/{total} tests passed")
        if passed == total:
            print("\n  ALL TESTS PASSED")
        else:
            print("\n  SOME TESTS FAILED")

        # Print gateway log excerpt for debugging.
        print("\n--- Gateway tollgate log (last 15 lines) ---")
        ssh(gw_ip, "tail", "-15", "/tmp/tollgate.log")

    finally:
        # ── Cleanup: cancel BOTH VMs ────────────────────────────────
        print("\n" + "=" * 60)
        print("  Cleanup — cancelling VMs")
        print("=" * 60)
        for sid, label in ((gw_sid, "gateway"), (cl_sid, "client")):
            if sid:
                print(f"  Cancelling {label} (service {sid})...")
                try:
                    client.cancel_vm(sid, immediate=True)
                    print(f"  {label} cancelled.")
                except Exception as exc:
                    print(f"  ERROR cancelling {label}: {exc}")


if __name__ == "__main__":
    main()
