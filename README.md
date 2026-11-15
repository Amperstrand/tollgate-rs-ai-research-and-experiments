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

> **Shared vocabulary:** the ecosystem [glossary](docs/design/glossary.md) — canonical terms across tollgate-rs, charger firmware, pecan, and provider adapters. Any repo adding a concept adds its glossary row in the same PR.

→ **Start here:** [tollgate-intro.md](docs/design/core/tollgate-intro.md) — goals, architecture, payment model, security.

> Implementation is well underway: the v1 compat layer (Go v1 TollGate
> protocol on OpenWrt), Cashu wallet with NUT-02/04/05/07/09/13/23
> compliance, Lightning payments, and OpenWrt packaging (.apk for 25.12+,
> .ipk legacy for 24.10) are all working and CI-tested. The Spilman
> channel design docs below describe the next phase.

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

## Implementation Status

The `v1_compat` layer implements the Go v1 TollGate wire protocol (HTTP
on port 2121, Nostr event advertisements, Cashu token payments, Lightning
invoices) and runs on OpenWrt routers:

- **Cashu wallet** — CDK-backed, NUT-02 (keysets + fees), NUT-04 (mint
  quotes), NUT-05 (melt), NUT-07 (checkstate with SSRF validation),
  NUT-09/13 (seed-only wallet restore), NUT-23 (BOLT11)
- **Lightning payments** — zero-balance clients pay via BOLT11 invoice;
  the server mints proofs and admits the peer through the Driver
- **Exactly-once grant registry** — concurrent Lightning polls cannot
  double-mint or double-grant
- **Startup reconciliation** — NUT-07 checkstate on ambiguous receives
  (crash recovery)
- **Per-deployment wallet seeds** — cryptographically random, persisted
  0600, survive restarts with NUT-13 derivation counters intact
- **OpenWrt packaging** — `.apk` (primary, 25.12+) and `.ipk` (legacy,
  24.10); portal vendored; CI regression-tested
- **VM E2E harness** — repeatable QEMU-based package + payment testing

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
    │     └── Cashu wallet (cdk-spilman based)
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
- [Cashu Spilman Channels](reference/cashu_spilman_channels/ARCHITECTURE.md) — Unidirectional payment channels for Cashu ecash
- [Cashu Protocol](https://cashu.space/) — Ecash protocol

## License

MIT
