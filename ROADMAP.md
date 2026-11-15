# TollGate RS — Roadmap

This roadmap tracks the evolution of TollGate RS from design-phase protocol
to production FIPS mesh networking with internet exit capabilities.

---

## Phase 0 — v1 Compat Layer (Complete)

Go v1 TollGate protocol compatibility on OpenWrt — the payment layer
that runs today while the FIPS-native design is finalized.

- [x] Cashu wallet (CDK): NUT-02/04/05/07/09/13/23
- [x] Lightning payments: BOLT11 invoice → mint → Driver admission
- [x] Exactly-once grant registry (concurrent pollers safe)
- [x] NUT-07 startup reconciliation (ambiguous receive recovery)
- [x] NUT-09/13 seed-only wallet restore (disaster recovery)
- [x] SSRF validation on checkstate (security)
- [x] OpenWrt packaging: .apk (25.12+) + .ipk (24.10 legacy)
- [x] VM E2E harness (QEMU, 24.10 + 25.12)
- [x] Real-mint integration (testnut.cashu.space, keyset fees)
- [x] Persistent wallet localstore (NUT-13 counter survives restarts)
- [ ] Bench E2E (physical router validation) ← next
- [ ] Payout flow (melt to Lightning address)
- [ ] m2 runtime modules (session API, NDS valve, wifi/ubus/uci)

---

## Phase 1 — FIPS Integration (Design Complete, Implementation Paused)

TollGate RS integrates with [FIPS](https://github.com/nicobao/fips) as the
ideal network layer: cryptographic peer authentication, encrypted forwarding,
self-organizing mesh routing, and rich per-link metrics.

- [x] Design documents: [peering-fips.md](docs/design/network-peering/peering-fips.md), [FIPS_FEATURE_REQUESTS.md](docs/design/FIPS_FEATURE_REQUESTS.md)
- [x] Core protocol design: pricing, metering, access control, payment channels
- [ ] Implement FIPS control socket features 1–4 (per-peer forwarding, bloom filters, traffic counters, lifecycle events)
- [ ] Docker-based integration tests (port from FIPS repo)
- [ ] QEMU-based multi-node test topologies
- [ ] Physical router lab validation (3+ GL-MT6000/MT3000 nodes)

---

## Phase 2 — FIPS Internet Exit / Tunneling

FIPS-only nodes gain the ability to reach the legacy internet through other
FIPS nodes that have WAN connectivity. This is the key capability that makes
FIPS viable as a primary network protocol.

- [ ] GRE tunnel PoC: 3 FIPS routers, 1 with internet, binary allow/deny (price = 0)
- [ ] TUN/TAP interface for bridging FIPS → legacy internet
- [ ] TollGate RS pricing on the tunnel interface (nft counters / tc / BPF)
- [ ] Test order: Docker → QEMU → physical routers
- [ ] Jump host pattern: `ssh -J user@npub.fips user@legacy.server`

---

## Phase 3 — Full FIPS-Only Network

Remove IP entirely from the internal mesh. Only loopback + FIPS + GRE tunnel
interfaces remain.

- [ ] Disable all IP internally (no DHCP, no IP firewall, no IP addresses)
- [ ] Multiple exit nodes with reputation/ratings (market for exit access)
- [ ] DNS resolution through FIPS
- [ ] Per-FIPS-instance pricing for heterogeneous peers (LoRa vs fiber)

---

## Phase 4 — Native Mobile Integration

TollGate runs natively on Android via FIPS, without Tauri/WebView.

- [ ] Native Android app: Rust core (nostril-native + Cashu NIP-60 + tokio) via `cargo-ndk`
- [ ] Kotlin/Jetpack Compose UI via UniFFI (no Tauri)
- [ ] FIPS-based push notifications (no FCM/APNS — direct Nostr events to Foreground Service)
- [ ] Validate Wi-Fi Direct for phone-to-router connections without hotspot bypass
- [ ] Gamification: ham radio style FIPS node "call sign" collection (opt-in)

---

## Future

- **microFIPS**: Add ESP32 as build target to main FIPS repo using feature flags (exclude NIM, ethernet). Eliminates separate microFIPS fork.
- **TTL/Ping proximity proof**: Physical proximity enforcement for pairing, DoS protection, and gamification.
- **Payment-aware routing**: Well-paying peers get favorable routing decisions in the mesh.
- **802.11s mesh backbone**: Router-to-router mesh using IEEE 802.11s (best-supported on MediaTek mt76 chipsets), with standard Wi-Fi AP for phone connections.
