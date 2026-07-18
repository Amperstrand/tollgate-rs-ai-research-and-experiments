#!/usr/bin/env python3
"""Deploy Rust TollGate to SHC VM and run PRTA test suite against it."""
import os, sys, time, subprocess, base64, json

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PRTA = os.path.join(os.path.dirname(REPO), "physical-router-test-automation")
BINARY = os.path.join(REPO, "target/x86_64-unknown-linux-musl/release/tollgate") if os.path.exists(os.path.join(REPO, "target/x86_64-unknown-linux-musl/release/tollgate")) else os.path.join(REPO, "target/release/tollgate")
TOLLTOP = os.path.join(REPO, "target/release/tolltop")
FAKE_MINT = os.path.join(REPO, "testing/bootstrap/fake-mint.py")
SSH_PUB = os.path.expanduser("~/.ssh/tollgate_cloud_key.pub")
SSH_PRIV = os.path.expanduser("~/.ssh/tollgate_cloud_key")
PORT = 2121
ROOT_PW = "tollgate"

sys.path.insert(0, os.environ.get("SHC_TOOLKIT_PATH", "/home/ubuntu/src/shc-toolkit"))
from shc_toolkit.client import SHCClient

def ssh(ip, cmd, timeout=30):
    return subprocess.run(
        ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
         "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR", "-i", SSH_PRIV, f"debian@{ip}", cmd],
        capture_output=True, timeout=timeout)

def scp(local, ip, remote, timeout=180):
    return subprocess.run(
        ["scp", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
         "-o", "LogLevel=ERROR", "-i", SSH_PRIV, local, f"debian@{ip}:{remote}"],
        capture_output=True, timeout=timeout)

def get_ip(d):
    if isinstance(d.get("ips"), list) and d["ips"]:
        return d["ips"][0].get("ip")
    return d.get("ip_address") or d.get("ip")

def main():
    with open(SSH_PUB) as f:
        pubkey = f.read().strip()

    c = SHCClient()
    print(f"Balance: ${float(c.get_account_balance()['credit'][0]['amount']):.2f}")

    print("Ordering SHC VM...")
    vm = c.order_vm(hostname=f"tg-prta-{int(time.time())}", package_id=81, pricing_id=245, ssh_key=pubkey)
    sid = vm.get("service_id") or vm.get("id")
    print(f"Ordered: {sid}")

    print("Waiting for provisioning...")
    time.sleep(15)
    ip = None
    for i in range(40):
        try:
            d = c.get_vm_detail(sid)
            ip = get_ip(d)
            st = d.get("provisioning_state", "?")
            if ip and st in ("ready", "active"):
                print(f"  Ready: {ip}")
                break
            print(f"  [{i+1}] {st}")
        except:
            print(f"  [{i+1}] ...")
        time.sleep(10)

    if not ip:
        print("FAILED"); c.cancel_vm(sid, immediate=True); return

    try:
        c.apply_ssh_key_live(sid, pubkey)
        time.sleep(5)

        print("Waiting for SSH...")
        for _ in range(15):
            r = ssh(ip, "echo OK", timeout=10)
            if r.returncode == 0 and b"OK" in r.stdout:
                print(f"  SSH OK: debian@{ip}")
                break
            time.sleep(10)

        # Enable root SSH for PRTA compatibility (PRTA hardcodes root@)
        print("Enabling root SSH for PRTA...")
        ssh(ip, f"echo 'root:{ROOT_PW}' | sudo chpasswd")
        ssh(ip, "sudo sed -i 's/#*PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config")
        ssh(ip, "sudo systemctl restart sshd")
        time.sleep(2)

        # Verify root SSH works
        r = subprocess.run(
            ["sshpass", "-e", "ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", "LogLevel=ERROR", f"root@{ip}", "echo ROOT_OK"],
            capture_output=True, timeout=10,
            env={**os.environ, "SSHPASS": ROOT_PW})
        if b"ROOT_OK" not in r.stdout:
            print("Root SSH failed! Trying sshpass install...")
            subprocess.run(["sudo", "apt-get", "install", "-y", "sshpass"], capture_output=True)
            r = subprocess.run(
                ["sshpass", "-e", "ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                 "-o", "LogLevel=ERROR", f"root@{ip}", "echo ROOT_OK"],
                capture_output=True, timeout=10,
                env={**os.environ, "SSHPASS": ROOT_PW})
        if b"ROOT_OK" in r.stdout:
            print("  Root SSH OK!")
        else:
            print(f"  Root SSH failed: {r.stderr.decode()[:200]}")
            return

        # No fake-mint needed — using testnut.cashu.exchange as mint
        print("Using testnut.cashu.exchange as mint (no fake-mint needed)")

        _gz = "/tmp/tollgate.gz"
        with open(_gz, "wb") as _f:
            subprocess.run(["gzip", "-c", BINARY], stdout=_f, check=True)
        print(f"Deploying tollgate ({os.path.getsize(BINARY)//1048576}MB → {os.path.getsize(_gz)//1048576}MB gz)...")
        scp(_gz, ip, "/tmp/tollgate.gz", timeout=300)
        ssh(ip, "gunzip -f /tmp/tollgate.gz && sudo mv /tmp/tollgate /usr/local/bin/tollgate && sudo chmod +x /usr/local/bin/tollgate")
        ssh(ip, "sudo ln -sf /usr/local/bin/tollgate /usr/sbin/tollgate-wrt")

        # Write config — listen on 2121 (PRTA BACKEND_PORT)
        yaml_cfg = f'''listen: "0.0.0.0:{PORT}"
unit: "milliseconds"
mints:
  - "https://testnut.cashu.exchange"
  - "https://testnut.cashu.exchange"
firewall: sets-only
metering_interval_secs: 5
v1_compat:
  metric: "milliseconds"
  step_size: 5000
  accepted_mints:
    - url: "https://testnut.cashu.exchange"
      price_per_step: 1
      unit: "sat"
      min_steps: 1
'''
        b64 = base64.b64encode(yaml_cfg.encode()).decode()
        ssh(ip, f"echo '{b64}' | base64 -d | sudo tee /etc/tollgate.yaml > /dev/null")

        # Also write Go-format config.json at /etc/tollgate/config.json for PRTA compatibility
        go_cfg = json.dumps({
            "config_version": "v0.0.7",
            "metric": "milliseconds",
            "step_size": 5000,
            "accepted_mints": [{
                "url": f"https://testnut.cashu.exchange",
                "min_balance": 0,
                "balance_tolerance_percent": 0,
                "payout_interval_seconds": 60,
                "min_payout_amount": 0,
                "price_per_step": 1,
                "price_unit": "sats",
                "purchase_min_steps": 0,
            }],
            "profit_share": [{"factor": 1.0, "identity": "owner"}],
        }, indent=2)
        ssh(ip, "sudo mkdir -p /etc/tollgate")
        ssh(ip, f"echo '{go_cfg}' | sudo tee /etc/tollgate/config.json > /dev/null")

        # Start tollgate
        print(f"Starting tollgate on port {PORT}...")
        ssh(ip, f"nohup /usr/local/bin/tollgate --config /etc/tollgate.yaml serve --listen 0.0.0.0:{PORT} > /tmp/tollgate.log 2>&1 &")
        time.sleep(4)

        # Verify
        r = ssh(ip, f"curl -s -o /dev/null -w '%{{http_code}}' http://127.0.0.1:{PORT}/")
        status = r.stdout.decode().strip()
        if status != "200":
            print(f"Server not responding (HTTP {status}). Log:")
            ssh(ip, "tail -20 /tmp/tollgate.log")
            return
        print(f"TollGate UP on port {PORT}!")

        # Run PRTA tests
        print(f"\n{'='*60}")
        print(f"  Running PRTA test_rust_v1_api.py against Rust backend")
        print(f"{'='*60}\n")

        env = os.environ.copy()
        env.update({
            "TOLLGATE_BACKEND": "rust",
            "TOLLGATE_SSH_HOST": ip,
            "TOLLGATE_SSH_USER": "root",
            "TOLLGATE_SSH_PASSWORD": ROOT_PW,
            "TOLLGATE_SSH_KEY": "",
            "TOLLGATE_ROUTER_ARCH": "x86_64",
            "TOLLGATE_TEST_MINT_URL": f"https://testnut.cashu.exchange",
            "TOLLGATE_LOCAL_MINT_URL": f"https://testnut.cashu.exchange",
            "TOLLGATE_CASHU_VENV": os.path.expanduser("~/.cashu-venv"),
            "TOLLGATE_VIRTUAL_LAB": "",
            "TOLLGATE_CLIENT_IP": ip,
            "TOLLGATE_CLIENT_MAC": "02:00:00:00:00:01",
        })

        result = subprocess.run(
            ["python3", "-m", "pytest", "tests/api/test_rust_v1_api.py",
             "-v", "--tb=short", "--no-header",
             "--timeout=120", "-o", "addopts="],
            cwd=PRTA,
            env=env,
            capture_output=True,
            text=True,
            timeout=600,
        )

        print("STDOUT:")
        print(result.stdout[-4000:])
        if result.stderr:
            print("\nSTDERR (last 1000):")
            print(result.stderr[-1000:])

        print(f"\nPRTA exit code: {result.returncode}")

    finally:
        print(f"\nCancelling VM {sid}...")
        try: c.cancel_vm(sid, immediate=True)
        except: pass

if __name__ == "__main__":
    main()
