# Physical Router E2E — Implementation Notes (2026-05-19)

Branch: `m4/ipk-physical-e2e`  
PR: https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments/pull/41  
CI run: https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments/actions/runs/26086005370

## What was implemented

### Package runtime fixes
- `V1Server` CLI renamed to `server` (alias `v1-server`) for Go-compatible init script
- Init script passes `--wallet cdk` for real Cashu on router
- CI builds with `--features nds` so `--valve nds` works when nodogsplash is installed

### Packaging
- Vendored Go v1 `99-tollgate-setup` uci-defaults (first-boot SSID, nodogsplash, LuCI :8080)
- Added `scripts/build-ipk-local.sh` for local mipsel/x86_64 builds
- Added `GET /pay` returning advertisement JSON (kind 10021) for smoke tests

### CI
- Added `x86_64` compile + package matrix rows
- **Build and Package** succeeded on PR #41

## Verified artifacts

| Architecture | Artifact | Size |
|--------------|----------|------|
| mipsel_24kc | `tollgate-wrt_41-merge.141.9fd1ee0_mipsel_24kc.ipk` | ~10.6 MB |
| x86_64 | `tollgate-wrt_41-merge.141.9fd1ee0_x86_64.ipk` | (downloaded OK) |

`download-rust-ci-artifact.sh m4/ipk-physical-e2e` resolves run `26086005370` and downloads mipsel artifact successfully.

IPK payload includes:
- `/usr/bin/tollgate-wrt`
- `/etc/init.d/tollgate-wrt`
- `/etc/uci-defaults/90-tollgate-captive-portal-symlink`, `99-tollgate-setup`
- `/etc/tollgate/tollgate-captive-portal-site/` (from Go v1 reference)

## Test execution status

### Physical router (mipsel_24kc @ 192.168.13.112)
**Blocked — router unreachable** (`ssh: Host is down`).

When router is online:

```bash
cd physical-router-test-automation
source ~/.tollgate-test-venv/bin/activate
TOLLGATE_BACKEND=rust \
TOLLGATE_ROUTER_ARCH=mipsel_24kc \
./scripts/deploy-rust-ci.sh m4/ipk-physical-e2e

# Verify
ssh root@$TOLLGATE_SSH_HOST 'curl -s http://127.0.0.1:2121/ | head -c 200'

# Full API campaign
./scripts/test-pr.sh --branch m4/ipk-physical-e2e --backend rust --test api
```

### Cloud lab (GCP)
**Blocked — GCP quota** (`IN_USE_ADDRESSES` limit 8 in europe-west1).

Artifact wait succeeded (x86_64 from run 26086005370). VM creation failed.

When quota available:

```bash
./scripts/cloud-lab.py submit --pr 41 --backend rust --wait
```

## Known API gaps vs physical-router-test-automation

| Area | Status | Notes |
|------|--------|-------|
| `GET /`, `/usage`, `/whoami`, `/balance`, `/ln-invoice` | Implemented | Covered by `v1_api_parity.rs` |
| `GET /pay` | Partial | Returns kind 10021 advertisement (200). Full 402 + `payment_request`/`qr_image` captive-portal flow not yet implemented — use `/ln-invoice` for Lightning |
| LuCI admin / CLI socket (:2050) | N/A | Tests marked `go_only`, auto-skipped for rust |
| `sessions.json` persistence | N/A | Go-only; rust uses in-memory/SQLite store |
| Real traffic valve | Partial | NDS valve when built with `nds` feature; no iptables (M4) |

## Recommended next steps

1. Power on mipsel_24kc lab router and run deploy + `make smoke` with `TOLLGATE_BACKEND=rust`
2. Free GCP address quota or use another region, then re-run `cloud-lab.py submit --pr 41 --backend rust`
3. If `/pay` 402 flow needed for phone tests, implement LN quote response on GET `/pay` when no active session
4. Merge PR #41 after physical smoke passes
