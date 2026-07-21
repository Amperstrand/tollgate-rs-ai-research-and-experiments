"""Shared helpers for SHC test scripts — automatic tunnel fallback.

When SHC blocks inbound traffic, this module:
1. Tries direct SSH to the VM
2. Falls back to Cloudflare Quick Tunnel (outbound HTTPS)
3. Sets up local port forwarding so HTTP tests work unchanged

Usage in test scripts:

    from shc_helpers import get_access

    ssh_host, ssh_port, http_base = get_access(service_id, test_port=2121)
    # SSH: subprocess.run(["ssh", "-p", str(ssh_port), f"debian@{ssh_host}", cmd])
    # HTTP: subprocess.run(["curl", f"{http_base}/whoami"])
"""
from __future__ import annotations

import logging
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.expanduser("~/src/shc-toolkit"))

log = logging.getLogger(__name__)


def _check_ssh(ip: str, port: int = 22, key: str = "~/.ssh/id_ed25519", timeout: int = 8) -> bool:
    try:
        r = subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
             "-o", f"ConnectTimeout={timeout}", "-o", "LogLevel=ERROR",
             "-i", os.path.expanduser(key), "-p", str(port), f"debian@{ip}", "echo OK"],
            capture_output=True, text=True, timeout=timeout + 5,
        )
        return "OK" in r.stdout
    except Exception:
        return False


def _setup_port_forward(ssh_host: str, ssh_port: int, test_port: int, key: str, socket: str | None = None) -> subprocess.Popen | None:
    opts = ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR", "-o", "ExitOnForwardFailure=yes",
            "-i", os.path.expanduser(key)]
    if socket:
        opts += ["-S", socket, "-O", "forward", "-L", f"{test_port}:localhost:{test_port}",
                 f"debian@{ssh_host}"]
        subprocess.run(opts + ["-p", str(ssh_port)], capture_output=True, timeout=10)
        return None
    opts += ["-N", "-L", f"{test_port}:localhost:{test_port}",
             "-p", str(ssh_port), f"debian@{ssh_host}"]
    try:
        proc = subprocess.Popen(opts, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(3)
        if proc.poll() is None:
            return proc
    except Exception:
        pass
    return None


def _setup_ssh_master(ssh_host: str, ssh_port: int, key: str) -> str:
    socket = "/tmp/ssh-tunnel-master"
    opts = ["ssh", "-M", "-S", socket, "-o", "ControlPersist=600",
            "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ConnectTimeout=15", "-o", "LogLevel=ERROR",
            "-i", os.path.expanduser(key), "-p", str(ssh_port), f"debian@{ssh_host}",
            "echo MASTER_OK"]
    try:
        r = subprocess.run(opts, capture_output=True, text=True, timeout=20)
        if "MASTER_OK" in r.stdout:
            return socket
    except Exception:
        pass
    return None


def get_access(
    service_id: int,
    test_port: int = 2121,
    ssh_key: str = "~/.ssh/id_ed25519",
    verbose: bool = True,
) -> tuple[str, int, str]:
    """Ensure SSH + HTTP access to a VM. Returns (ssh_host, ssh_port, http_base).

    Tries direct SSH first. If blocked, establishes Cloudflare tunnel +
    local port forward so HTTP tests work unchanged.
    """
    from shc_toolkit.client import SHCClient

    c = SHCClient()
    detail = c.get_vm(service_id)
    ip = detail["ips"][0]["ip"] if detail.get("ips") else None
    if not ip:
        raise RuntimeError(f"VM {service_id} has no IP")

    if verbose:
        print(f"  VM {service_id} at {ip}")

    if _check_ssh(ip, key=ssh_key):
        if verbose:
            print("  Direct SSH works")
        return ip, 22, f"http://{ip}:{test_port}"

    if verbose:
        print("  Direct SSH blocked — establishing tunnel...")

    from shc_toolkit.tunnel import ensure_ssh_access

    ssh_host, ssh_port = ensure_ssh_access(
        service_id, local_port=2222, key=ssh_key, verbose=verbose,
    )

    master = _setup_ssh_master(ssh_host, ssh_port, ssh_key)
    if master:
        if verbose:
            print("  SSH master connection established (reliable tunnel)")
        _setup_port_forward(ssh_host, ssh_port, test_port, ssh_key, socket=master)
        if verbose:
            print(f"  Port forward: localhost:{test_port} → VM:{test_port}")
    else:
        _setup_port_forward(ssh_host, ssh_port, test_port, ssh_key)
        if verbose:
            print(f"  Port forward (no master): localhost:{test_port} → VM:{test_port}")

    http_base = f"http://localhost:{test_port}"
    if verbose:
        print(f"  HTTP tests via: {http_base}")

    return ssh_host, ssh_port, http_base
