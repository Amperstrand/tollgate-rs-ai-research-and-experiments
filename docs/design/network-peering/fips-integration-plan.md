# M6: FIPS Mesh Integration — Research & Implementation Plan

## Status: Research Complete, Implementation Planned

## Current State

### What Exists
- **FIPS repo** (`Amperstrand/fips`): Rust implementation v0.5.0-dev, checked out at `/home/ubuntu/src/fips/`
- **Design docs**: `peering-fips.md` (integration architecture), `FIPS_FEATURE_REQUESTS.md` (11 features)
- **FIPS-style control socket**: `control_server.rs` already serves `NodeStatus` on a Unix socket
- **IpAdapter**: `adapter.rs` (546 LOC) — nftables-based, validated in M4 docker tests

### What's Missing (4 Critical Features from FIPS_FEATURE_REQUESTS.md)

| # | Feature | Complexity | Status in FIPS |
|---|---------|-----------|----------------|
| 1 | Per-peer forwarding policy (`local_only`/`full`) | Medium | Needs FIPS control socket command |
| 2 | Bloom filter exclusion (inferred from policy) | Medium | Derived from #1 |
| 3 | Per-peer traffic counter livestream | Low | Data exists (`LinkStats`), needs socket subscription |
| 4 | Peer lifecycle events (connect/disconnect) | Low | Needs socket event stream |

## Integration Architecture

```
┌──────────── FIPS Node ────────────┐
│  FIPS daemon (independent binary)  │
│  ├── Noise IK handshakes           │
│  ├── Encrypted forwarding (FMP)    │
│  ├── Spanning tree routing         │
│  ├── Bloom filter distribution     │
│  └── MMP metrics per link          │
│                                    │
│  Unix Control Socket ──────────┐   │
└────────────────────────────────┼───┘
                                 │
┌────────────────────────────────▼───┐
│  tollgate-net (our binary)          │
│  ├── FipsAdapter (new)              │
│  │   ├── Subscribe to counters      │
│  │   ├── Set forwarding policy      │
│  │   ├── Receive lifecycle events   │
│  │   └── Read MMP metrics           │
│  ├── Driver (existing)              │
│  │   ├── Session state machine      │
│  │   ├── Metering loop              │
│  │   └── Pricing engine (M5)       │
│  └── BootstrapWallet / SpilmanService │
└────────────────────────────────────┘
```

## Implementation Phases

### Phase 1: FipsAdapter (depends on FIPS features 1-4)
1. Create `FipsAdapter` struct implementing the same interface as `IpAdapter`
2. Connect to FIPS control socket
3. Subscribe to per-peer counter livestream (replaces nftables counter reads)
4. Subscribe to peer lifecycle events (replaces HTTP transport's Announce)
5. Set forwarding policy via control socket (replaces nftables allow/deny)

### Phase 2: FIPS Transport (replaces HTTP polling)
1. Register FSP port for TollGate messages
2. Send/receive CBOR messages directly via FSP (no HTTP framing overhead)
3. Use FIPS node addresses instead of IP addresses for peer identity

### Phase 3: Dynamic Pricing Integration (uses M5 + FIPS MMP)
1. Feed MMP metrics (SRTT, loss, ETX) into the pricing adjustment engine
2. Quality-tiered pricing based on link metrics
3. Payment-aware routing feedback to FIPS

## Key Design Decisions

| Decision | Resolution |
|----------|-----------|
| Communication | Unix control socket (both binaries on same host) |
| FipsAdapter trait | Same interface as IpAdapter: `allow(ip)`, `deny(ip)`, `read_counters(ip)` |
| Peer identity | FIPS node_addr (16 bytes) → map to TollGate PeerId (33-byte pubkey) |
| Metering | Livestream subscription (push) instead of polling nftables |
| Access control | FIPS forwarding policy replaces nftables rules |

## Dependencies

- FIPS control socket API must expose features 1-4 (critical)
- FIPS must be running on the same host (Unix socket)
- No network changes needed — FIPS handles all transport

## Estimated Effort

| Phase | Effort | Blocking? |
|-------|--------|-----------|
| Phase 1: FipsAdapter | Large (1-2 weeks) | Blocked by FIPS features 1-4 |
| Phase 2: FSP Transport | Medium (3-5 days) | Blocked by FIPS feature 6 |
| Phase 3: Pricing Integration | Small (1-2 days) | Blocked by Phase 1 + M5 (done) |

## Next Steps

1. Check FIPS repo for control socket implementation status
2. Prototype FipsAdapter connecting to FIPS control socket
3. Test on SHC with 3+ FIPS nodes
