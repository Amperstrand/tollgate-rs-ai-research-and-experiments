#!/usr/bin/env python3
"""Deploy Rust TollGate to SHC and run smoke tests."""
import os, sys, time, subprocess, base64, re

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

def ssh_ok(ip, user):
    r = subprocess.run(["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
         "-o", "ConnectTimeout=5", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"{user}@{ip}", "echo", "OK"],
        capture_output=True, timeout=10)
    return r.returncode == 0 and b"OK" in r.stdout

def main():
    with open(SSH_PUB) as f:
        pubkey = f.read().strip()
    c = SHCClient()
    print(f"Balance: ${float(c.get_account_balance()['credit'][0]['amount']):.2f}")

    print("Ordering SHC VM...")
    vm = c.order_vm(hostname=f"tg-{int(time.time())}", package_id=81, pricing_id=245, ssh_key=pubkey)
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
            print(f"  [{i+1}] {st} ip={ip or '?'}")
        except Exception as e:
            print(f"  [{i+1}] {str(e)[:50]}")
        time.sleep(10)

    if not ip:
        print("FAILED"); c.cancel_vm(sid, immediate=True); return

    print("Applying SSH key live...")
    try: c.apply_ssh_key_live(sid, pubkey)
    except: pass
    time.sleep(5)

    user = None
    print("Waiting for SSH...")
    for _ in range(12):
        for u in ["debian", "root"]:
            if ssh_ok(ip, u):
                user = u; break
        if user:
            print(f"  SSH OK: {user}@{ip}"); break
        time.sleep(10)

    if not user:
        print("SSH failed"); c.cancel_vm(sid, immediate=True); return

    ssh = lambda *cmd: subprocess.run(["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
        "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"{user}@{ip}"] + list(cmd),
        capture_output=True, timeout=30)

    try:
        _gz = "/tmp/tollgate.gz"
        with open(_gz, "wb") as _f:
            subprocess.run(["gzip", "-c", BINARY], stdout=_f, check=True)
        print(f"Deploying binary ({os.path.getsize(BINARY)//1048576}MB → {os.path.getsize(_gz)//1048576}MB gz)...")
        subprocess.run(["scp", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR", "-i", SSH_PRIV, _gz, f"{user}@{ip}:/tmp/tollgate.gz"], check=True, timeout=300)
        ssh("gunzip", "-f", "/tmp/tollgate.gz")
        ssh("sudo", "mv", "/tmp/tollgate", "/usr/local/bin/tollgate")
        ssh("sudo", "chmod", "+x", "/usr/local/bin/tollgate")

        cfg = f'listen: "0.0.0.0:{PORT}"\nunit: "milliseconds"\nmints:\n  - "{TEST_MINT}"\nfirewall: sets-only\nmetering_interval_secs: 5\nv1_compat:\n  metric: "milliseconds"\n  step_size: 5000\n  accepted_mints:\n    - url: "{TEST_MINT}"\n      price_per_step: 1\n      unit: "sat"\n      min_steps: 1\n'
        ssh(f"echo '{base64.b64encode(cfg.encode()).decode()}' | base64 -d | sudo tee /etc/tollgate.yaml > /dev/null")

        print(f"Starting tollgate on :{PORT}...")
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

        print(f"\n{'='*50}\n  SMOKE TESTS: {ip}:{PORT}\n{'='*50}\n")
        tests = [
            ("GET / ad", f"http://{ip}:{PORT}/", "200", r'"kind"'),
            ("GET /whoami", f"http://{ip}:{PORT}/whoami", "200", r"mac="),
            ("GET /usage", f"http://{ip}:{PORT}/usage", "404", ""),
            ("GET /balance", f"http://{ip}:{PORT}/balance", "404", ""),
            ("GET /pay", f"http://{ip}:{PORT}/pay", "200", r'"kind"'),
            ("POST / bad token", f"http://{ip}:{PORT}/", "400", ""),
            ("v2 exchange (POST)", f"-X POST {ip}:{PORT}/tollgate/v1/exchange", "400", ""),
        ]
        p = f_ = 0
        for name, url, wc, wb in tests:
            try:
                if "bad token" in name:
                    r = subprocess.run(["curl", "-s", "-w", "\\n%{http_code}", "-X", "POST", "-d", "fake", url],
                        capture_output=True, timeout=10)
                else:
                    r = subprocess.run(["curl", "-s", "-w", "\\n%{http_code}", url], capture_output=True, timeout=10)
                out = r.stdout.decode().strip().split("\n")
                code = out[-1]; body = "\n".join(out[:-1])
                ok = code == wc and (not wb or re.search(wb, body))
                print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {code}")
                p += 1 if ok else 0; f_ += 0 if ok else 1
            except Exception as e:
                print(f"  [FAIL] {name}: {e}"); f_ += 1
        print(f"\n  {p} passed, {f_} failed")
    finally:
        print(f"\nCancelling VM {sid}...")
        try: c.cancel_vm(sid, immediate=True)
        except: pass

if __name__ == "__main__":
    main()
