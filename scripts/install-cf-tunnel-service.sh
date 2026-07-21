#!/bin/bash
# Install on VM via SSH (once tunnel is up) or console paste:
#
#   ssh debian@vm 'bash -s' < scripts/install-cf-tunnel-service.sh
#
# Or via console (one line at a time using "Type into VM" button)

set -e

# Find cloudflared
CF=$(which cloudflared 2>/dev/null || echo /usr/local/bin/cloudflared)
if [ ! -x "$CF" ]; then
    echo "ERROR: cloudflared not found"
    exit 1
fi

# Kill any existing manual cloudflared
pkill -f 'cloudflared tunnel' 2>/dev/null || true
sleep 1

# Install systemd service
sudo tee /etc/systemd/system/cf-ssh-tunnel.service > /dev/null << 'UNIT'
[Unit]
Description=Cloudflare Quick Tunnel for SSH
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStartPre=/bin/rm -f /tmp/cf-tunnel.log /tmp/cf-tunnel-url.txt
ExecStart=/bin/sh -c '/usr/local/bin/cloudflared tunnel --url tcp://localhost:22 2>&1 | tee /tmp/cf-tunnel.log; while true; do sleep 1; done'
Restart=always
RestartSec=5
Environment=NO_AUTO_UPDATE=1

[Install]
WantedBy=multi-user.target
UNIT

sudo systemctl daemon-reload
sudo systemctl enable --now cf-ssh-tunnel

echo ""
echo "=== Cloudflare Tunnel Service Installed ==="
echo "Status: $(systemctl is-active cf-ssh-tunnel)"
echo ""
echo "Waiting for tunnel URL..."
sleep 15

URL=$(grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' /tmp/cf-tunnel.log | head -1)
if [ -n "$URL" ]; then
    echo "$URL" | sudo tee /tmp/cf-tunnel-url.txt
    echo ""
    echo "Tunnel URL: $URL"
    echo ""
    echo "To connect from your machine:"
    echo "  cloudflared access tcp --hostname ${URL#https://} --url localhost:2222"
    echo "  ssh -p 2222 debian@localhost"
else
    echo "URL not yet available — check: cat /tmp/cf-tunnel.log"
fi
