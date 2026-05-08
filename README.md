# tollgate-rs

![tollgate-rs banner](docs/design/tollgate-rs-banner.png)

Rust implementation of the [TollGate](https://github.com/OpenTollGate)
protocol — autonomous, device-to-device payment for metered resource
delivery, built on Cashu ecash and Spilman payment channels.

This repo contains:

- **tollgate-protocol** — the wire format and lifecycle, defined in
  the design documents under `docs/design/`. Resource-agnostic.
- **tollgate-core** — Rust library implementing the protocol's
  resource-agnostic logic (channels, metering, pricing, access control).
- **tollgate-net** — binary that uses `tollgate-core` to (re)sell
  network access over traditional IP networks or a self-organizing mesh
  such as [FIPS](https://github.com/nicobao/fips). This is the first
  deployment of TollGate.

A constrained-device variant (`tollgate-net-esp32`) lives in a separate
project and consumes the same `tollgate-core`.

→ **Start here:** [tollgate-intro.md](docs/design/core/tollgate-intro.md) — goals, architecture, payment model, security.

## Implementation Status

Milestones M1–M2 are complete. M3 (Spilman payment channels) has a working
demonstration against a public Cashu mint.

| Milestone | Description | Status |
|-----------|-------------|--------|
| M1 | Core Types, Protocol Codec, Peer State Machine | ✅ Complete |
| M2 | Bootstrap Token Payment, CDK Wallet Integration | ✅ Complete |
| M3 | Spilman Payment Channels (demo) | 🔄 In Progress |
| M4 | tollgate-net — IP Peering Deployment | Open |
| M5 | Dynamic Pricing and Operator Controls | Open |
| M6 | FIPS Mesh Integration | Open |
| M7 | Production Hardening and Packaging | Open |

### What's Implemented

- **`tollgate-core`** — Full CBOR codec (minicbor), protocol messages, peer state machine,
  quota exhaustion (Terminate/Restrict/Allow), metering types
- **`tollgate-net`** — Binary with CDK-based Cashu wallet for bootstrap token operations
  (receive, verify, send, balance) and Spilman channel demo
- **Protocol trace visualization** — Interactive Mermaid diagrams of protocol flows,
  deployed to GitHub Pages with clickable per-step channel state popups

### Dependencies and Attribution

| Component | Source | What We Use It For |
|-----------|--------|--------------------|
| **`cdk`** v0.16 | [cashubtc/cdk](https://github.com/cashubtc/cdk) (crates.io) | Cashu wallet operations: token receive/verify, balance tracking, mint HTTP client |
| **`cdk-spilman`** v0.15.1 | [SatsAndSports/cashu_spilman_channels](https://github.com/SatsAndSports/cashu_spilman_channels) (git) | Spilman channel primitives: `construct_proofs()`, `parse_keyset_info_from_json()`, `KeysetInfo` |
| **`cashu`** | [cashubtc/cdk](https://github.com/cashubtc/cdk) at rev `63866dc6` (git) | Cashu proof types (`Proof`, `Secret`, `Amount`) used in test artifacts |
| **`minicbor`** | crates.io | CBOR encoding for wire protocol (ADR-0002: integer keys, no_std compatible) |
| **`tollgate-core`** | Our implementation | Protocol codec, state machine, pricing types, quota exhaustion |
| **`SpilmanChannelManager`** | Our implementation (`spilman_wallet.rs`) | HTTP orchestration: keyset fetch, mint quote, blind signature → proof construction. Calls `cdk-spilman` for crypto primitives |
| **Channel lifecycle test** | Our implementation (`spilman_integration.rs`) | End-to-end Spilman channel flow against a live mint, with trace artifact generation for visualization |

**What's ours vs what's library code:**

- The **Spilman crypto** (blinding, proof construction, DLEQ verification) comes from
  `cdk-spilman` (SatsAndSports). We do not implement the cryptographic primitives.
- The **channel orchestration** (HTTP calls to mint, quote polling, balance update signing,
  cooperative close flow) is our code in `SpilmanChannelManager`. It wires the library
  primitives together into a working channel lifecycle.
- The **protocol codec and state machine** in `tollgate-core` is entirely our implementation.
- The **visualization** (Mermaid diagrams, interactive state popups, denomination bars) is
  our implementation, generated from test trace artifacts.

## Overview

TollGate enables any device that delivers a metered resource to another
device to charge for that service using Cashu ecash micropayments.
Devices negotiate prices, open payment channels, and settle autonomously
based on observed usage — no accounts, no registration, no central
billing authority.

TollGate is not a network protocol. It is a payment layer that operates
alongside any system where peers are authenticated and can deliver
resources to each other. `tollgate-core` is resource-agnostic — it
works for network forwarding, electricity metering, fluid delivery, or
any metered resource.

## How It Works

Each peer charges its own rate for delivering resources. Prices can be
positive, zero, or negative. Payment flows through Cashu Spilman
channels — unidirectional payment channels with streaming micropayments.
Two channels per peer pair (one per direction) enable bidirectional
payment with netting.

![Hop-by-Hop Payment](docs/design/core/diagrams/hop-by-hop.svg)

> **The operator's margin is the spread between what they charge for delivery and what they pay their peers.**

Each hop is its own independent commercial relationship. Clients don't
need path knowledge; operators earn the margin between what they buy
upstream and what they sell downstream.

At each metering interval (default: 5 seconds), both sides exchange
metering reports. The net debtor signs a single balance update — only the
delta moves.

## Key Properties

- **Hop-by-hop payment** — each peer pays its direct neighbor, no path
  knowledge needed
- **Per-peer pricing** — every relationship has its own price, per product,
  per mint, dynamically adjustable
- **Resource-agnostic** — core library works for bytes, watt-hours,
  milliliters, or any metered unit
- **Cashu-native** — Spilman channels for streaming payment, regular tokens
  for bootstrap
- **Offline-resilient** — balance updates don't need the mint; channels
  survive connectivity loss
- **Operator sovereignty** — the operator controls pricing, accepted mints,
  and peering policy

## Project Structure

```
tollgate-rs/
├── docs/
│   └── design/
│       ├── core/              Core protocol design documents (resource-agnostic)
│       └── network-peering/   Network-specific integration (IP, FIPS)
└── reference/
    ├── fips/                  FIPS mesh network (ideal substrate)
    ├── tollgate-module-basic-go/  TollGate v1 (Go, OpenWrt)
    └── cashu_spilman_channels/    Cashu Spilman channel implementation
```

## Design Documents

Start with the [introduction](docs/design/core/tollgate-intro.md), then
follow the reading order in the [design README](docs/design/README.MD).

| Document | Description |
| -------- | ----------- |
| [tollgate-intro.md](docs/design/core/tollgate-intro.md) | Goals, architecture, payment model, security |
| [tollgate-pricing.md](docs/design/core/tollgate-pricing.md) | Dual pricing (time + units), products, dynamic adjustment |
| [tollgate-protocol.md](docs/design/core/tollgate-protocol.md) | CBOR wire protocol, interval flow, negotiation |
| [tollgate-payment-channels.md](docs/design/core/tollgate-payment-channels.md) | Spilman channel lifecycle, rollover, netting |
| [tollgate-bootstrap.md](docs/design/core/tollgate-bootstrap.md) | Bootstrap tokens, bootstrap-only mode |
| [tollgate-access-control.md](docs/design/core/tollgate-access-control.md) | Access gates, access levels, FIPS bloom filter visibility |
| [tollgate-metering.md](docs/design/core/tollgate-metering.md) | Metering: counters, calibration, transit loss resolution |
| [tollgate-configuration.md](docs/design/core/tollgate-configuration.md) | YAML configuration reference |
| [peering-ip.md](docs/design/network-peering/peering-ip.md) | Traditional IP network integration |
| [peering-fips.md](docs/design/network-peering/peering-fips.md) | FIPS mesh network integration |
| [FIPS_FEATURE_REQUESTS.md](docs/design/FIPS_FEATURE_REQUESTS.md) | Required FIPS changes |

## Architecture

`tollgate-core` is a resource-agnostic library; deployments are binaries
that consume it and provide a `Wallet` and a `ResourceAdapter`.

```
tollgate-core (lib)              Pure logic, resource-agnostic
    │
    ├── tollgate-net (this binary)  Network forwarding, feature-flagged per OS
    │     ├── Linux / macOS / Windows / OpenWrt
    │     ├── FIPS or IP network adapter
    │     ├── Cashu wallet (cdk — bootstrap tokens)
    │     └── Spilman channels (cdk-spilman — payment channels)
    │
    └── tollgate-net-esp32 (separate project)
          ├── ESP-IDF / constrained runtime
          └── Custom wallet + resource adapter
```

`tollgate-core` defines traits (`Wallet`, `ResourceAdapter`,
`ProductSelector`) that consumers provide. `tollgate-net` targets
Linux, macOS, Windows, and OpenWrt with feature flags for OS-specific
differences. ESP32 lives in a separate project due to fundamentally
different runtime constraints.

## Prior Work

- [TollGate v1](https://github.com/OpenTollGate/tollgate-module-basic-go) — Go implementation for OpenWrt, tree topology, Cashu token payments
- [FIPS](https://github.com/nicobao/fips) — Self-organizing encrypted mesh network
- [SatsAndSports/cashu_spilman_channels](https://github.com/SatsAndSports/cashu_spilman_channels) — Cashu Spilman channel crypto primitives (used as `cdk-spilman` dependency)
- [CDK](https://github.com/cashubtc/cdk) — Cashu Development Kit, official Rust Cashu wallet library
- [Cashu Protocol](https://cashu.space/) — Ecash protocol

## License

MIT
