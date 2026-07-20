#!/usr/bin/env python3
"""Establish SSH access to an SHC VM when inbound traffic is blocked.

Uses Cloudflare Quick Tunnel (outbound HTTPS) as a reverse tunnel.
Falls back to noVNC console automation for initial setup.

Usage:
    python3 scripts/establish_tunnel.py --vm 1077
    python3 scripts/establish_tunnel.py --vm 1077 --local-port 2222

Prerequisites:
    - SHC_API_KEY environment variable
    - cloudflared binary at /tmp/cf-binary or /usr/local/bin/cloudflared
    - playwright + pytesseract installed (for console fallback)
    - shc-toolkit at ~/src/shc-toolkit
"""
import argparse
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.expanduser("~/src/shc-toolkit"))


def check_ssh_direct(ip, port=22, key="~/.ssh/id_ed25519", timeout=8):
    try:
        r = subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", f"ConnectTimeout={timeout}", "-o", "LogLevel=ERROR",
             "-i", os.path.expanduser(key), f"debian@{ip}", "echo SSH_DIRECT_OK"],
            capture_output=True, text=True, timeout=timeout + 5)
        return "SSH_DIRECT_OK" in r.stdout
    except Exception:
        return False


def get_vm_credentials(service_id):
    from shc_toolkit.mcp_client import SHCMCPClient
    c = SHCMCPClient()
    creds = c.get_vm_credentials(service_id)
    return creds["user"], creds["password"]


def get_vm_ip(service_id):
    from shc_toolkit.client import SHCClient
    c = SHCClient()
    detail = c.get_vm(service_id)
    ips = detail.get("ips", [])
    return ips[0]["ip"] if ips else None


def find_cloudflared():
    for path in ["/tmp/cf-binary", "/usr/local/bin/cloudflared", os.path.expanduser("~/.local/bin/cloudflared")]:
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


def console_ensure_tunnel(service_id, username, password):
    """Use noVNC console to check/start cloudflared tunnel on VM."""
    from shc_toolkit.mcp_client import SHCMCPClient
    from playwright.sync_api import sync_playwright
    import pytesseract
    from PIL import Image

    def ocr(path):
        return pytesseract.image_to_string(Image.open(path), config="--psm 6").strip()

    def send_text(page, text):
        page.evaluate(f"""() => {{
            document.getElementById('clipboard-textarea').value = {repr(text)};
            document.getElementById('btn-send').click();
        }}""")
        page.wait_for_timeout(800)

    mcp = SHCMCPClient()
    session = mcp.create_console_session(service_id)
    url = session["console_url"]

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1024, "height": 768})
        page.goto(url, wait_until="networkidle", timeout=15000)
        page.wait_for_timeout(5000)

        page.keyboard.press("Enter")
        page.wait_for_timeout(2000)
        send_text(page, f"{username}\n")
        page.wait_for_timeout(3000)
        send_text(page, f"{password}\n")
        page.wait_for_timeout(8000)
        send_text(page, "clear\n")
        page.wait_for_timeout(2000)

        send_text(page, "pgrep -x cloudflared >/dev/null && echo CF_RUNNING || echo CF_STOPPED\n")
        page.wait_for_timeout(3000)
        page.screenshot(path="/tmp/_tunnel_check.png")
        text = ocr("/tmp/_tunnel_check.png")

        if "cf_stopped" in text.lower() or "cf_running" not in text.lower():
            print("  Starting cloudflared tunnel on VM...")
            send_text(page, "nohup cloudflared tunnel --url tcp://localhost:22 > /tmp/cf-tunnel.log 2>&1 &\n")
            page.wait_for_timeout(15000)

        # Get tunnel URL via transfer service
        send_text(page, "clear\n")
        page.wait_for_timeout(1000)
        send_text(page, "grep -o 'https://[a-z0-9-]*\\.trycloudflare\\.com' /tmp/cf-tunnel.log | head -1 | curl -s -L -F 'f:1=<-' http://ix.io 2>&1\n")
        page.wait_for_timeout(10000)
        page.screenshot(path="/tmp/_tunnel_url.png")
        text = ocr("/tmp/_tunnel_url.png")

        tunnel_url = None
        for line in text.split("\n"):
            line = line.strip().replace(" ", "")
            if "ix.io" in line and len(line) < 30:
                tunnel_url = line
                break

        if tunnel_url:
            r = subprocess.run(["curl", "-s", "-L", tunnel_url], capture_output=True, text=True, timeout=10)
            cf_url = r.stdout.strip()
            if "trycloudflare" in cf_url:
                print(f"  Tunnel URL: {cf_url}")
                browser.close()
                return cf_url

        # Fallback: try to read URL directly from screenshot
        for line in text.split("\n"):
            line = line.strip().replace(" ", "")
            if "trycloudflare" in line.lower():
                print(f"  Tunnel URL (from OCR): {line}")
                browser.close()
                return line

        browser.close()
        print("  WARNING: Could not extract tunnel URL from console")
        return None


def start_local_client(cf_binary, tunnel_hostname, local_port):
    """Start cloudflared access tcp locally and return process."""
    proc = subprocess.Popen(
        [cf_binary, "access", "tcp", "--hostname", tunnel_hostname, "--url", f"localhost:{local_port}"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(8)
    return proc


def ensure_ssh_key(service_id, username, password, local_port, key="~/.ssh/id_ed25519"):
    """Add our SSH key to the VM via console if needed."""
    pubkey = open(os.path.expanduser(key + ".pub")).read().strip()
    from shc_toolkit.mcp_client import SHCMCPClient
    from playwright.sync_api import sync_playwright
    import pytesseract
    from PIL import Image

    def ocr(path):
        return pytesseract.image_to_string(Image.open(path), config="--psm 6").strip()

    def send_text(page, text):
        page.evaluate(f"""() => {{
            document.getElementById('clipboard-textarea').value = {repr(text)};
            document.getElementById('btn-send').click();
        }}""")
        page.wait_for_timeout(800)

    mcp = SHCMCPClient()
    session = mcp.create_console_session(service_id)
    url = session["console_url"]

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1024, "height": 768})
        page.goto(url, wait_until="networkidle", timeout=15000)
        page.wait_for_timeout(5000)
        page.keyboard.press("Enter")
        page.wait_for_timeout(2000)
        send_text(page, f"{username}\n")
        page.wait_for_timeout(3000)
        send_text(page, f"{password}\n")
        page.wait_for_timeout(8000)
        send_text(page, "clear\n")
        page.wait_for_timeout(2000)

        send_text(page, f"echo '{pubkey}' >> ~/.ssh/authorized_keys\n")
        page.wait_for_timeout(3000)
        send_text(page, "sudo sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && sudo systemctl restart ssh\n")
        page.wait_for_timeout(5000)
        browser.close()
        print("  SSH key added to VM")


def main():
    parser = argparse.ArgumentParser(description="Establish SSH tunnel to SHC VM")
    parser.add_argument("--vm", type=int, required=True, help="SHC VM service ID")
    parser.add_argument("--local-port", type=int, default=2222, help="Local port for tunnel")
    parser.add_argument("--key", default="~/.ssh/id_ed25519", help="SSH key to use")
    args = parser.parse_args()

    print(f"=== Establishing tunnel to VM {args.vm} ===\n")

    # Step 1: Get VM info
    ip = get_vm_ip(args.vm)
    if not ip:
        print("ERROR: Could not get VM IP")
        sys.exit(1)
    print(f"VM IP: {ip}")

    # Step 2: Check if SSH is directly reachable (fast path)
    print("\nStep 1: Checking direct SSH access...")
    if check_ssh_direct(ip, key=args.key):
        print(f"  SSH works directly! Use: ssh -i {args.key} debian@{ip}")
        return
    print("  Direct SSH blocked. Need tunnel.")

    # Step 3: Find cloudflared binary
    cf_binary = find_cloudflared()
    if not cf_binary:
        print("  Downloading cloudflared...")
        subprocess.run(["wget", "-q", "-O", "/tmp/cf-binary",
                        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64"],
                       check=True)
        subprocess.run(["chmod", "+x", "/tmp/cf-binary"], check=True)
        cf_binary = "/tmp/cf-binary"
    print(f"  cloudflared: {cf_binary}")

    # Step 4: Get VM credentials
    print("\nStep 2: Getting VM credentials...")
    username, password = get_vm_credentials(args.vm)
    print(f"  User: {username}")

    # Step 5: Ensure cloudflared tunnel is running on VM + get URL
    print("\nStep 3: Ensuring cloudflared tunnel on VM...")
    tunnel_url = console_ensure_tunnel(args.vm, username, password)
    if not tunnel_url:
        print("ERROR: Could not establish tunnel on VM")
        sys.exit(1)

    # Extract hostname from URL
    tunnel_hostname = tunnel_url.replace("https://", "").rstrip("/")
    print(f"  Tunnel hostname: {tunnel_hostname}")

    # Step 6: Start local cloudflared client
    print(f"\nStep 4: Starting local tunnel client (port {args.local_port})...")
    # Kill any existing
    subprocess.run(["pkill", "-f", "cf-binary.*access"], capture_output=True)
    subprocess.run(["pkill", "-f", "cloudflared.*access"], capture_output=True)
    time.sleep(2)
    proc = start_local_client(cf_binary, tunnel_hostname, args.local_port)

    # Step 7: Test SSH through tunnel
    print(f"\nStep 5: Testing SSH through tunnel...")
    try:
        r = subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR",
             "-i", os.path.expanduser(args.key),
             "-p", str(args.local_port), "debian@localhost", "echo TUNNEL_SSH_OK"],
            capture_output=True, text=True, timeout=20)
        if "TUNNEL_SSH_OK" in r.stdout:
            print("  SSH THROUGH TUNNEL WORKS!")
        else:
            print(f"  SSH auth failed: {r.stderr[:100]}")
            print("  Adding SSH key via console...")
            ensure_ssh_key(args.vm, username, password, args.local_port, args.key)
            time.sleep(3)
            r = subprocess.run(
                ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                 "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR",
                 "-i", os.path.expanduser(args.key),
                 "-p", str(args.local_port), "debian@localhost", "echo TUNNEL_SSH_OK"],
                capture_output=True, text=True, timeout=20)
            if "TUNNEL_SSH_OK" in r.stdout:
                print("  SSH THROUGH TUNNEL WORKS! (after key add)")
            else:
                print(f"  Still failing: {r.stderr[:100]}")
                sys.exit(1)
    except Exception as e:
        print(f"  SSH test failed: {e}")
        sys.exit(1)

    print(f"\n{'='*60}")
    print(f"  TUNNEL ESTABLISHED SUCCESSFULLY!")
    print(f"{'='*60}")
    print(f"\n  Connect with:")
    print(f"    ssh -i {args.key} -p {args.local_port} debian@localhost")
    print(f"\n  For SHC test scripts, use:")
    print(f"    TOLLGATE_SSH_HOST=localhost TOLLGATE_SSH_PORT={args.local_port}")
    print(f"\n  Tunnel URL: {tunnel_url}")
    print(f"  VM IP: {ip}")
    print(f"  Local cloudflared PID: {proc.pid}")


if __name__ == "__main__":
    main()
