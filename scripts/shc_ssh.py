"""SHC SSH access with automatic Cloudflare tunnel fallback.

When direct SSH to a VM fails (inbound blocked), this module automatically
establishes a Cloudflare Quick Tunnel via noVNC console and routes SSH
through it.

Usage in test scripts:
    from shc_ssh import ensure_access
    ip, port = ensure_access(service_id)  # Returns (host, port) for SSH
    # Then use ssh -p {port} debian@{ip}
"""
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.expanduser("~/src/shc-toolkit"))


def check_ssh(ip, port=22, key="~/.ssh/id_ed25519", timeout=8):
    try:
        r = subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", f"ConnectTimeout={timeout}", "-o", "LogLevel=ERROR",
             "-i", os.path.expanduser(key), "-p", str(port), f"debian@{ip}", "echo OK"],
            capture_output=True, text=True, timeout=timeout + 5)
        return "OK" in r.stdout
    except Exception:
        return False


def find_cloudflared():
    for path in ["/tmp/cf-binary", "/usr/local/bin/cloudflared",
                 os.path.expanduser("~/.local/bin/cloudflared")]:
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    path = "/tmp/cf-binary"
    subprocess.run(["wget", "-q", "-O", path,
                    "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64"],
                   check=True, timeout=60)
    subprocess.run(["chmod", "+x", path], check=True)
    return path


def ensure_access(service_id, local_port=2222, key="~/.ssh/id_ed25519", verbose=True):
    """Ensure SSH access to a VM. Returns (host, port).

    Tries direct SSH first. If blocked, establishes Cloudflare tunnel.
    """
    from shc_toolkit.client import SHCClient
    c = SHCClient()
    detail = c.get_vm(service_id)
    ip = detail["ips"][0]["ip"] if detail.get("ips") else None
    if not ip:
        raise RuntimeError(f"VM {service_id} has no IP")

    if verbose:
        print(f"  VM {service_id} at {ip}")

    if check_ssh(ip, key=key):
        if verbose:
            print(f"  Direct SSH works")
        return ip, 22

    if verbose:
        print(f"  Direct SSH blocked — establishing tunnel...")

    tunnel_url = _establish_tunnel_on_vm(service_id, verbose)
    if not tunnel_url:
        raise RuntimeError("Could not establish tunnel on VM")

    hostname = tunnel_url.replace("https://", "").rstrip("/")
    cf_binary = find_cloudflared()

    subprocess.run(["pkill", "-f", "cf-binary.*access"], capture_output=True)
    subprocess.run(["pkill", "-f", "cloudflared.*access"], capture_output=True)
    time.sleep(2)

    subprocess.Popen(
        [cf_binary, "access", "tcp", "--hostname", hostname,
         "--url", f"localhost:{local_port}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    time.sleep(8)

    _ensure_ssh_key(service_id, key, verbose)

    if check_ssh("localhost", port=local_port, key=key):
        if verbose:
            print(f"  Tunnel SSH works on localhost:{local_port}")
        return "localhost", local_port

    raise RuntimeError("Tunnel SSH failed after setup")


def _establish_tunnel_on_vm(service_id, verbose=True):
    from shc_toolkit.mcp_client import SHCMCPClient
    from playwright.sync_api import sync_playwright
    import pytesseract
    from PIL import Image

    def ocr(path):
        return pytesseract.image_to_string(Image.open(path), config="--psm 6").strip().lower()

    def send_text(page, text):
        page.evaluate(f"""() => {{
            document.getElementById('clipboard-textarea').value = {repr(text)};
            document.getElementById('btn-send').click();
        }}""")
        page.wait_for_timeout(800)

    mcp = SHCMCPClient()
    creds = mcp.get_vm_credentials(service_id)
    username, password = creds["user"], creds["password"]

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

        send_text(page, "pgrep -x cloudflared >/dev/null && echo RUNNING || echo STOPPED\n")
        page.wait_for_timeout(3000)
        page.screenshot(path="/tmp/_tc.png")
        if "stopped" in ocr("/tmp/_tc.png"):
            if verbose:
                print("  Starting cloudflared on VM...")
            send_text(page, "nohup cloudflared tunnel --url tcp://localhost:22 > /tmp/cf-tunnel.log 2>&1 &\n")
            page.wait_for_timeout(15000)

        send_text(page, "clear\n")
        page.wait_for_timeout(1000)
        send_text(page, "grep -o 'https://[a-z0-9-]*\\.trycloudflare\\.com' /tmp/cf-tunnel.log | head -1 | curl -s -L -F 'f:1=<-' http://ix.io 2>&1\n")
        page.wait_for_timeout(10000)
        page.screenshot(path="/tmp/_tu.png")
        text = ocr("/tmp/_tu.png")

        tunnel_url = None
        for line in text.split("\n"):
            line = line.strip().replace(" ", "")
            if "ix.io" in line and len(line) < 30:
                r = subprocess.run(["curl", "-s", "-L", line], capture_output=True, text=True, timeout=10)
                if "trycloudflare" in r.stdout:
                    tunnel_url = r.stdout.strip()
                    break

        if not tunnel_url:
            for line in text.split("\n"):
                line = line.strip().replace(" ", "")
                if "trycloudflare" in line:
                    tunnel_url = line
                    break

        browser.close()
        return tunnel_url


def _ensure_ssh_key(service_id, key, verbose=True):
    from shc_toolkit.mcp_client import SHCMCPClient
    from playwright.sync_api import sync_playwright

    pubkey = open(os.path.expanduser(key + ".pub")).read().strip()

    def send_text(page, text):
        page.evaluate(f"""() => {{
            document.getElementById('clipboard-textarea').value = {repr(text)};
            document.getElementById('btn-send').click();
        }}""")
        page.wait_for_timeout(800)

    mcp = SHCMCPClient()
    creds = mcp.get_vm_credentials(service_id)
    session = mcp.create_console_session(service_id)

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1024, "height": 768})
        page.goto(session["console_url"], wait_until="networkidle", timeout=15000)
        page.wait_for_timeout(5000)
        page.keyboard.press("Enter")
        page.wait_for_timeout(2000)
        send_text(page, f"{creds['user']}\n")
        page.wait_for_timeout(3000)
        send_text(page, f"{creds['password']}\n")
        page.wait_for_timeout(8000)
        send_text(page, "clear\n")
        page.wait_for_timeout(2000)
        send_text(page, f"echo '{pubkey}' >> ~/.ssh/authorized_keys\n")
        page.wait_for_timeout(3000)
        send_text(page, "sudo sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config && sudo systemctl restart ssh\n")
        page.wait_for_timeout(5000)
        browser.close()
        if verbose:
            print("  SSH key added to VM")
