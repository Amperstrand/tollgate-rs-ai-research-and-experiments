# Physical Router Test Plan

**Status**: Plan only (not yet implemented)  
**Milestone**: M2.5 → M4  
**Prerequisites**: CI-built .ipk artifacts for all architectures, 2+ OpenWrt routers

## 1. Test Environment

### Hardware
- **Router A** (Go v1 reference): OpenWrt router running `tollgate-module-basic-go` — acts as upstream TollGate server
- **Router B** (Rust DUT): OpenWrt router running `tollgate-net` from .ipk — acts as client (M2.5) and/or server
- **Laptop**: For SSH control, packet capture, and test orchestration
- **Network**: Ethernet between routers (for initial bringup), WiFi for production scenarios

### Software on Laptop
- Python test runner (or shell scripts) that SSHes into both routers
- `curl` for HTTP API testing
- `tcpdump` / Wireshark for packet verification

### Software on Routers
- Router A: Go v1 installed and configured per its README
- Router B: tollgate-rs .ipk installed via `opkg`
- Both routers: Cashu wallet pre-funded with test tokens

## 2. Test Phases

### Phase 1: Server Smoke Tests (Router B as server)

These verify the `/ln-invoice` + `/` + `/usage` endpoints work on real hardware.

| # | Test | Method | Pass Criteria |
|---|------|--------|---------------|
| 1.1 | Install .ipk | `opkg install tollgate-net_*.ipk` | Installs without error, binary at `/usr/bin/tollgate-net` |
| 1.2 | Start server | SSH into Router B, run with config | Server listens on `:2121` |
| 1.3 | Fetch advertisement | `curl http://router-b:2121/` | Returns valid Nostr kind 10021 JSON |
| 1.4 | POST token payment | `curl -X POST -d "cashuA..." http://router-b:2121/` | Returns kind 1022 session event with allotment tag |
| 1.5 | Poll usage | `curl http://router-b:2121/usage` | Returns `usage/allotment` format (e.g., `0/60000`) |
| 1.6 | POST /ln-invoice | `curl -X POST -d '{"amount":100}' http://router-b:2121/ln-invoice` | Returns `{quote, request, expiry}` |
| 1.7 | GET /ln-invoice | `curl "http://router-b:2121/ln-invoice?quote_id=..."` | Returns quote status, eventually ISSUED |
| 1.8 | Session created | `curl http://router-b:2121/usage` after payment | Non-zero allotment, usage tracking begins |

### Phase 2: Client Mode Tests (Router B as client of Router A)

These verify the Rust V1Client can pay a Go v1 upstream server.

| # | Test | Method | Pass Criteria |
|---|------|--------|---------------|
| 2.1 | Discover upstream | Router B runs upstream_detector, probes Router A's gateway IP | `DiscoveredUpstream` parsed correctly |
| 2.2 | Fetch advertisement | V1Client `connect()` fetches ad from Router A:2121 | Pricing options extracted, compatible mint found |
| 2.3 | Send payment | V1Client sends Cashu token to Router A:2121 | Go v1 accepts token, returns session event |
| 2.4 | Track usage | V1Client polls `/usage` | Usage increases as traffic flows |
| 2.5 | Auto-renew | Let session approach exhaustion | V1Client detects threshold, sends renewal payment |
| 2.6 | Session recovery | Restart Router B's tollgate-net mid-session | Re-attaches to existing session without new payment |

### Phase 3: Server-to-Server Interop (Router A ↔ Router B)

Both routers act as servers, each accepting payments from the other.

| # | Test | Method | Pass Criteria |
|---|------|--------|---------------|
| 3.1 | Rust server accepts Go client | Configure Go v1 on Router A as client of Router B | Go v1 can pay Router B's Rust server |
| 3.2 | Go server accepts Rust client | Router B's V1Client pays Router A's Go server | Full cycle works |
| 3.3 | Mixed payment types | Test both Cashu token (`POST /`) and LN invoice (`/ln-invoice`) against both servers | Both payment methods work on Rust server |
| 3.4 | Error handling | Send invalid tokens to both servers | Both return appropriate error responses |
| 3.5 | Concurrent sessions | Multiple clients paying simultaneously | No race conditions, all sessions tracked correctly |

### Phase 4: Network Integration (Future — M4)

| # | Test | Method | Pass Criteria |
|---|------|--------|---------------|
| 4.1 | WiFi discovery | Router B scans WiFi and finds Router A's TollGate SSID | Vendor IEs parsed, pricing extracted |
| 4.2 | WiFi STA connect | Router B connects to Router A's AP | DHCP lease obtained, gateway reachable |
| 4.3 | Automatic payment | Router B discovers → pays → gets online | End-to-end without manual intervention |
| 4.4 | Traffic gating | Verify actual traffic is blocked/unblocked | iptables/nftables rules work correctly |
| 4.5 | Multi-hop | Three routers in a chain | Payments propagate, middle router earns margin |

## 3. Test Automation

### Approach
Write tests as shell scripts callable from the laptop, not installed on the routers.

```
tests/
├── router/
│   ├── common.sh              # SSH helpers, wait-for patterns
│   ├── phase1-server-smoke.sh # Phase 1 tests
│   ├── phase2-client-mode.sh  # Phase 2 tests  
│   ├── phase3-interop.sh      # Phase 3 tests
│   └── README.md              # How to set up the test environment
```

### Test Script Pattern
```bash
#!/bin/bash
# tests/router/phase1-server-smoke.sh
source "$(dirname "$0")/common.sh"

ROUTER_B="root@192.168.1.2"
TOKEN="cashuA..." # pre-funded test token

test_1_3_fetch_advertisement() {
    local resp
    resp=$(ssh "$ROUTER_B" "curl -s http://localhost:2121/")
    assert_json_field "$resp" "kind" "10021"
}
```

### CI Integration
These tests cannot run in CI (require physical hardware). They are:
- Run manually before release milestones
- Documented in `tests/router/README.md`
- Version-controlled alongside the code

## 4. Prerequisites Checklist

Before Phase 1 can begin:

- [ ] .ipk builds for target architecture (Router B's arch)
- [ ] Go v1 reference server running on Router A
- [ ] Test Cashu tokens pre-funded on both routers' wallets
- [ ] Network connectivity between laptop ↔ both routers
- [ ] SSH access to both routers
- [ ] Router B's config file (`/etc/tollgate/config.toml`) with:
  - Server mode enabled
  - Accepted mints configured
  - Pricing configured (matching test tokens)
  - Wallet seed/mnemonic for test tokens

## 5. Open Questions

1. **Valve implementation**: StubValve doesn't gate traffic. Phase 4 tests requiring actual traffic control need iptables/nftables. This is M4 scope.
2. **Cashu mint for testing**: Need a reliable test mint. Options: `testnut.cashu.exchange` (public), or local `cdk-mintd`. Go v1 tests use testnut.
3. **LN invoice testing**: Phase 1.6-1.7 requires a Lightning-enabled mint. testnut supports BOLT11.
4. **Architecture mismatch**: Router B may be MIPS (mips_24kc) or ARM. CI must produce the correct .ipk. Currently CI produces: arm_cortex-a7, aarch64_cortex-a53, aarch64_cortex-a72, mips_24kc, mipsel_24kc.
5. **Go v1 config parity**: Rust server must accept the same Cashu token format and return the same Nostr event format as Go v1. Any format differences would break interop.
