#!/usr/bin/env python3
"""Extensive TollGate test suite on SHC — extracts PRTA test scenarios.

Creates a VM, deploys Rust TollGate, runs 8 test scenarios adapted from
physical-router-test-automation/tests/api/test_rust_v1_api.py.
"""
import os, sys, time, subprocess, base64, re, json

sys.path.insert(0, os.environ.get("SHC_TOOLKIT_PATH", "/home/ubuntu/src/shc-toolkit"))
from shc_toolkit.client import SHCClient

BINARY = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "tollgate")
SSH_PUB = os.path.expanduser("~/.ssh/tollgate_cloud_key.pub")
SSH_PRIV = os.path.expanduser("~/.ssh/tollgate_cloud_key")
TEST_MINT = "https://testnut.cashu.exchange"
PORT = 2121

def get_ip(d):
    if isinstance(d.get("ips"), list) and d["ips"]:
        return d["ips"][0].get("ip")
    return d.get("ip_address") or d.get("ip")

def ssh_cmd(ip, user, *cmd):
    return subprocess.run(
        ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
         "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"{user}@{ip}"] + list(cmd),
        capture_output=True, timeout=30)

def curl(url, *args):
    r = subprocess.run(["curl", "-s", "-w", "\n%{http_code}"] + list(args) + [url],
        capture_output=True, timeout=10)
    out = r.stdout.decode().strip().split("\n")
    code = out[-1] if out else "000"
    body = "\n".join(out[:-1])
    return code, body

def test_result(name, passed, detail=""):
    status = "PASS" if passed else "FAIL"
    print(f"  [{status}] {name}" + (f" — {detail[:100]}" if detail and not passed else ""))
    return passed

def main():
    with open(SSH_PUB) as f:
        pubkey = f.read().strip()
    c = SHCClient()
    print(f"Balance: ${float(c.get_account_balance()['credit'][0]['amount']):.2f}")

    print("Ordering SHC VM...")
    vm = c.order_vm(hostname=f"tg-ext-{int(time.time())}", package_id=81, pricing_id=245, ssh_key=pubkey)
    sid = vm.get("service_id") or vm.get("id")
    print(f"Ordered: {sid}")

    print("Waiting for provisioning...")
    time.sleep(15)
    ip = None
    for i in range(30):
        try:
            d = c.get_vm_detail(sid)
            ip = get_ip(d)
            st = d.get("provisioning_state", "?")
            if ip and st in ("ready", "active"):
                print(f"  Ready: {ip}")
                break
            print(f"  [{i+1}] {st}")
        except Exception as e:
            print(f"  [{i+1}] ...")
        time.sleep(10)

    if not ip:
        print("FAILED"); c.cancel_vm(sid, immediate=True); return

    print("Applying SSH key...")
    try: c.apply_ssh_key_live(sid, pubkey)
    except: pass
    time.sleep(5)

    user = None
    for _ in range(12):
        for u in ["debian", "root"]:
            r = ssh_cmd(ip, u, "echo", "OK")
            if r.returncode == 0 and b"OK" in r.stdout:
                user = u; break
        if user: break
        time.sleep(10)
    if not user:
        print("SSH failed"); c.cancel_vm(sid, immediate=True); return

    def ssh(*cmd):
        return ssh_cmd(ip, user, *cmd)

    try:
        print(f"Deploying binary ({os.path.getsize(BINARY)//1048576}MB)...")
        subprocess.run(["scp", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR", "-i", SSH_PRIV, BINARY, f"{user}@{ip}:/tmp/tollgate"], check=True, timeout=600)
        ssh("sudo", "mv", "/tmp/tollgate", "/usr/local/bin/tollgate")
        ssh("sudo", "chmod", "+x", "/usr/local/bin/tollgate")

        cfg = f'listen: "0.0.0.0:{PORT}"\nunit: "milliseconds"\nmints:\n  - "{TEST_MINT}"\nfirewall: sets-only\nmetering_interval_secs: 5\nv1_compat:\n  metric: "milliseconds"\n  step_size: 5000\n  accepted_mints:\n    - url: "{TEST_MINT}"\n      price_per_step: 1\n      unit: "sat"\n      min_steps: 1\n'
        b64 = base64.b64encode(cfg.encode()).decode()
        ssh(f"echo '{b64}' | base64 -d | sudo tee /etc/tollgate.yaml > /dev/null")
        ssh(f"nohup /usr/local/bin/tollgate --config /etc/tollgate.yaml serve --listen 0.0.0.0:{PORT} > /tmp/tollgate.log 2>&1 &")

        print("Waiting for server...")
        for _ in range(20):
            time.sleep(2)
            r = subprocess.run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", f"http://{ip}:{PORT}/"],
                capture_output=True, timeout=5)
            if r.stdout.decode().strip() == "200":
                print("  Server UP!"); break
        else:
            print("Server failed"); ssh("tail", "-20", "/tmp/tollgate.log"); return

        BASE = f"http://{ip}:{PORT}"
        XFF = "-H" f"X-Forwarded-For: 10.0.0.42"
        results = []

        print(f"\n{'='*60}")
        print(f"  EXTENSIVE TEST SUITE (adapted from PRTA test_rust_v1_api.py)")
        print(f"  Target: {ip}:{PORT}")
        print(f"{'='*60}\n")

        # Test 1: Advertisement
        print("Test 1: Advertisement (GET /)")
        code, body = curl(f"{BASE}/")
        try:
            data = json.loads(body)
            tags = {t[0]: t[1:] for t in data.get("tags", []) if isinstance(t, list) and t}
            has_kind = data.get("kind") == 10021
            has_metric = "metric" in tags
            has_step = "step_size" in tags
            has_price = "price_per_step" in tags
            results.append(test_result("Advertisement kind:10021", has_kind, body[:100]))
            results.append(test_result("Advertisement has metric tag", has_metric))
            results.append(test_result("Advertisement has step_size tag", has_step))
            results.append(test_result("Advertisement has price_per_step tag", has_price))
        except Exception as e:
            results.append(test_result("Advertisement parse", False, str(e)))

        # Test 2: Whoami
        print("\nTest 2: Whoami (GET /whoami)")
        code, body = curl(f"{BASE}/whoami", "-H", "X-Forwarded-For: 10.0.0.42")
        results.append(test_result("Whoami HTTP 200", code == "200", f"got {code}"))
        results.append(test_result("Whoami mac= format", "mac=" in body, body[:80]))

        # Test 3: Usage (no session)
        print("\nTest 3: Usage (GET /usage, no session)")
        code, body = curl(f"{BASE}/usage", "-H", "X-Forwarded-For: 10.0.0.42")
        results.append(test_result("Usage 404 without session", code == "404", f"got {code}"))

        # Test 4: Balance (no session)
        print("\nTest 4: Balance (GET /balance, no session)")
        code, body = curl(f"{BASE}/balance", "-H", "X-Forwarded-For: 10.0.0.42")
        results.append(test_result("Balance 404 without session", code == "404", f"got {code}"))

        # Test 5: POST invalid token
        print("\nTest 5: Payment rejection (POST / with fake token)")
        code, body = curl(f"{BASE}/", "-X", "POST", "-d", "cashuBfake_token_not_real")
        results.append(test_result("POST fake token rejected (400/402)", code in ("400", "402"), f"got {code}"))

        # Test 6: POST empty body
        print("\nTest 6: Payment rejection (POST / with empty body)")
        code, body = curl(f"{BASE}/", "-X", "POST", "-d", "")
        results.append(test_result("POST empty body rejected", code in ("400", "402"), f"got {code}"))

        # Test 7: LN Invoice
        print("\nTest 7: LN Invoice (POST /ln-invoice)")
        code, body = curl(f"{BASE}/ln-invoice", "-X", "POST", "-d", json.dumps({"amount": 8}), "-H", "Content-Type: application/json")
        try:
            data = json.loads(body)
            has_quote = "quote" in data or "invoice" in data
            results.append(test_result("LN invoice HTTP 200", code == "200", f"got {code}"))
            results.append(test_result("LN invoice has quote/invoice", has_quote, body[:100]))
        except Exception:
            results.append(test_result("LN invoice response", code in ("200", "500", "503"), f"got {code}: {body[:80]}"))

        # Test 8: LN Invoice status
        print("\nTest 8: LN Invoice status (GET /ln-invoice?quote=test)")
        code, body = curl(f"{BASE}/ln-invoice?quote=test-quote-123")
        results.append(test_result("LN invoice status returns response", code in ("200", "400", "500"), f"got {code}"))

        # Test 9: v2 exchange endpoint
        print("\nTest 9: v2 Exchange (POST /tollgate/v1/exchange)")
        code, body = curl(f"{BASE}/tollgate/v1/exchange", "-X", "POST", "-d", "")
        results.append(test_result("v2 exchange responds (200/400)", code in ("200", "400"), f"got {code}"))

        # Test 10: /pay alias
        print("\nTest 10: /pay alias (GET /pay)")
        code, body = curl(f"{BASE}/pay")
        try:
            data = json.loads(body)
            results.append(test_result("/pay returns advertisement", data.get("kind") == 10021, body[:80]))
        except Exception:
            results.append(test_result("/pay returns valid JSON", False, body[:80]))

        # Test 11: Server process health
        print("\nTest 11: Server process health")
        r = ssh("pgrep", "-c", "tollgate")
        proc_count = r.stdout.decode().strip()
        results.append(test_result("TollGate process running", proc_count and int(proc_count) > 0, f"procs={proc_count}"))

        # Summary
        passed = sum(results)
        total = len(results)
        print(f"\n{'='*60}")
        print(f"  RESULTS: {passed}/{total} passed ({100*passed//total}%)")
        print(f"{'='*60}")

        # Server log tail
        print("\nServer log:")
        ssh("tail", "-5", "/tmp/tollgate.log")

    finally:
        print(f"\nCancelling VM {sid}...")
        try: c.cancel_vm(sid, immediate=True)
        except: pass

if __name__ == "__main__":
    main()
