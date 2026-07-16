#!/usr/bin/env python3
"""Deploy Rust TollGate to SHC and run smoke tests."""
import os, sys, time, subprocess, base64, re, socket

sys.path.insert(0, os.environ.get("SHC_TOOLKIT_PATH", "/home/ubuntu/src/shc-toolkit"))

from shc_toolkit.client import SHCClient

BINARY = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "tollgate")
SSH_PUB = os.path.expanduser("~/.ssh/tollgate_cloud_key.pub")
SSH_PRIV = os.path.expanduser("~/.ssh/tollgate_cloud_key")
TEST_MINT = "https://testnut.cashu.exchange"
PORT = 4747

def wait_for_ssh(ip, timeout=300):
    """Direct SSH probe — more robust than wait_for_provisioning_healthy."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            s = socket.create_connection((ip, 22), timeout=5)
            s.close()
            # SSH port open — try actual SSH
            r = subprocess.run(
                ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                 "-o", "ConnectTimeout=5", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"root@{ip}", "echo", "OK"],
                capture_output=True, timeout=10
            )
            if r.returncode == 0 and b"OK" in r.stdout:
                return True
        except Exception:
            pass
        time.sleep(10)
    return False

def main():
    with open(SSH_PUB) as f:
        pubkey = f.read().strip()

    c = SHCClient()
    bal = c.get_account_balance()
    print(f"Balance: ${float(bal['credit'][0]['amount']):.2f}")

    print("Ordering SHC VM...")
    vm = c.order_vm(hostname=f"tg-test-{int(time.time())}", package_id=81, pricing_id=245, ssh_key=pubkey)
    sid = vm.get("service_id") or vm.get("id")
    print(f"Ordered: service_id={sid}")

    print("Waiting 20s for provisioning to register...")
    time.sleep(20)

    print("Polling for IP address...")
    ip = None
    for _ in range(30):
        try:
            d = c.get_vm_detail(sid)
            ip = d.get("ip_address") or d.get("ip")
            status = d.get("status", "?")
            if ip:
                print(f"  Got IP: {ip} (status={status})")
                break
            print(f"  status={status}, no IP yet...")
        except Exception as e:
            print(f"  Detail error: {e}")
        time.sleep(10)

    if not ip:
        print("FAILED: No IP after polling. Cleaning up.")
        c.cancel_vm(sid, immediate=True)
        return

    print(f"Waiting for SSH on {ip}:22...")
    if not wait_for_ssh(ip, timeout=180):
        print("SSH didn't come up. Trying to apply key live...")
        try:
            c.apply_ssh_key_live(sid, pubkey)
            time.sleep(10)
            if not wait_for_ssh(ip, timeout=60):
                print("FAILED: SSH unreachable. Cleaning up.")
                c.cancel_vm(sid, immediate=True)
                return
        except Exception as e:
            print(f"Key apply failed: {e}. Cleaning up.")
            c.cancel_vm(sid, immediate=True)
            return

    ssh = ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
           "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"root@{ip}"]
    scp = lambda src, dst: subprocess.run(["scp", "-o", "StrictHostKeyChecking=no",
           "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR",
           "-i", SSH_PRIV, src, dst], check=True, timeout=180)

    try:
        sz = os.path.getsize(BINARY) // 1024 // 1024
        print(f"Copying binary ({sz}MB)...")
        scp(BINARY, f"root@{ip}:/usr/local/bin/tollgate")
        subprocess.run(ssh + ["chmod +x /usr/local/bin/tollgate"], check=True, timeout=10)

        cfg = f'listen: "0.0.0.0:{PORT}"\nunit: "milliseconds"\nmints:\n  - "{TEST_MINT}"\nfirewall: sets-only\nmetering_interval_secs: 5\nv1_compat:\n  metric: "milliseconds"\n  step_size: 5000\n  accepted_mints:\n    - url: "{TEST_MINT}"\n      price_per_step: 1\n      unit: "sat"\n      min_steps: 1\n'
        b64 = base64.b64encode(cfg.encode()).decode()
        subprocess.run(ssh + [f"echo '{b64}' | base64 -d > /etc/tollgate.yaml"], check=True, timeout=10)

        print(f"Starting tollgate serve on :{PORT}...")
        subprocess.run(ssh + [f"nohup /usr/local/bin/tollgate serve --config /etc/tollgate.yaml --listen 0.0.0.0:{PORT} > /tmp/tollgate.log 2>&1 &"], check=False, timeout=10)

        print("Waiting for server...")
        up = False
        for _ in range(20):
            time.sleep(2)
            r = subprocess.run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", f"http://{ip}:{PORT}/"], capture_output=True, timeout=5)
            if r.stdout.decode().strip() == "200":
                up = True; break
        if not up:
            print("Server didn't respond. Logs:")
            subprocess.run(ssh + ["tail -30 /tmp/tollgate.log"], check=False)
            return

        print(f"\n{'='*50}\n  SMOKE TESTS: {ip}:{PORT}\n{'='*50}\n")
        tests = [
            ("GET / advertisement", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/"], "200", r'"kind":\s*10021'),
            ("GET /whoami", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/whoami"], "200", r"mac="),
            ("GET /usage", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/usage"], "404", ""),
            ("GET /balance", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/balance"], "404", ""),
            ("GET /pay alias", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/pay"], "200", r'"kind":\s*10021'),
            ("POST / invalid", ["curl", "-s", "-w", "\\n%{http_code}", "-X", "POST", "-d", "fake", f"http://{ip}:{PORT}/"], "400", ""),
            ("v2 exchange", ["curl", "-s", "-w", "\\n%{http_code}", f"http://{ip}:{PORT}/tollgate/v1/exchange"], "400", ""),
        ]
        p = f_ = 0
        for name, cmd, want_code, want_body in tests:
            try:
                r = subprocess.run(cmd, capture_output=True, timeout=10)
                out = r.stdout.decode().strip().split("\n")
                code = out[-1]; body = "\n".join(out[:-1])
                ok = code == want_code and (not want_body or re.search(want_body, body))
                print(f"  [{'PASS' if ok else 'FAIL'}] {name}: HTTP {code}")
                if not ok and body: print(f"         {body[:120]}")
                p += ok; f_ += not ok
            except Exception as e:
                print(f"  [FAIL] {name}: {e}"); f_ += 1
        print(f"\n  {p} passed, {f_} failed")
        subprocess.run(ssh + ["tail -3 /tmp/tollgate.log"], check=False)
    finally:
        print(f"\nCancelling VM {sid}...")
        c.cancel_vm(sid, immediate=True)
        print("Cancelled.")

if __name__ == "__main__":
    main()
