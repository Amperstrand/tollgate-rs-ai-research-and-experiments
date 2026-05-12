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

Milestones M1–M3 are in progress. M3 (Spilman payment channels) has a working
in-browser demo with real cryptographic operations against a public Cashu mint.

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
- **In-browser Spilman channel demo** — Real ECDH, Schnorr signatures, and blinded mint
  interactions against testnut.cashu.exchange, all running in the browser with no build step.
  Channel crypto delegated to cdk-wasm (Wave C). Spending condition witnesses for cooperative
  close. No unilateral close, no DLEQ verification, no persistence.

### Live Demos

Both demos run entirely in the browser with no build step:

| Demo | URL | What It Shows |
|------|-----|---------------|
| **Protocol Traces** | [GitHub Pages](https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/) | Auto-generated Mermaid sequence diagrams from Rust integration tests. Interactive state popups, balance timelines, educational walkthroughs with quizzes. |
| **Spilman Channel (real crypto)** | [GitHub Pages](https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/) | Alice (buyer) and Charlie (seller) execute a full Spilman channel lifecycle in your browser — real ECDH, real Schnorr signatures, real Cashu blinded mint interactions against testnut.cashu.exchange. No build step, no npm, no server. Just open and click. |

The spilman-real demo is also at `docs/private/demos/spilman-real/` — serve with any HTTP server (`python3 -m http.server`).

### Compare and Contrast: Spilman Channel Demos

Our goal: get a working Cashu Spilman channel running in a browser, in a simple educational way, so anyone can see how channel payments work without installing anything.

**What we built on**: The demo calls low-level cdk-wasm bindings (compiled from the same [SatsAndSports/cashu_spilman_channels](https://github.com/SatsAndSports/cashu_spilman_channels) Rust crate) for channel crypto operations. The channel state machine and mint HTTP orchestration are our own JS code. The mint wrappers follow the [Cashu NUT specs](https://github.com/cashubtc/nuts).

**What exists and how we differ**:

| | **Our demo** | **SatsAndSports examples** [[1]](https://github.com/SatsAndSports/cashu_spilman_channels/tree/main/examples) | **cashu-ts** [[2]](https://github.com/cashubtc/cashu-ts) | **Option A simulator** |
|---|---|---|---|---|
| **Where it runs** | Browser (any OS, no install) | Terminal (Rust, Node.js, Python, or Go) | Browser or Node.js | Browser |
| **Real crypto** | Yes — cdk-wasm (WASM compiled from Rust) + @noble/curves for keygen | Yes — cdk-spilman (Rust native) or WasmSpilmanBridge | Yes — @noble/curves | No — simulated SHA-256 only |
| **Real mint** | Yes — testnut.cashu.exchange (public, auto-paying) | Yes — local test mint (cdk-spilman-test-mintd) | No channel support yet | No — no mint interaction |
| **Architecture** | Low-level WASM bindings + hand-rolled JS orchestration | WasmSpilmanBridge / WasmSpilmanClientBridge classes manage full lifecycle | Library API | N/A |
| **Server/Client** | Both wallets in same page, direct function calls | Real HTTP server (Express/Axum) + separate client process | N/A | N/A |
| **Close paths** | Cooperative close only | Cooperative + unilateral + timeout (via bridge) | No channel support | Cooperative + unilateral + timeout (simulated) |
| **DLEQ verification** | Not implemented | Via `verify_proof_dleq` | Not for channels | N/A |
| **Spending conditions** | ✅ SIG_ALL witness with 2-of-2 P2PK (Wave C) | ✅ Managed by bridge | N/A | N/A |
| **What you see** | Alice and Charlie wallets, token flow, signature details, mint requests | ASCII art (pay-per-character), terminal logs | Library API calls | Step-by-step walkthrough with data boxes |
| **Educational** | Shows each phase, what tokens are used, what the signature commits to | Shows protocol flow via terminal output | Not a demo — it's a library | Highly educational — 20-step walkthrough with explanations |
| **Setup** | Open URL, click button | Clone, build, start server + client, two terminals | npm install, write code | Open URL, click through steps |

Sources:
- [1] [SatsAndSports/cashu_spilman_channels/examples](https://github.com/SatsAndSports/cashu_spilman_channels/tree/main/examples) — Reference implementations in Rust (Axum), TypeScript (Express), Python (Flask), and Go. Each runs a "pay-per-character ASCII art" server where a client opens a Spilman channel and pays per request. These are the closest existing demos to ours.
- [2] [cashubtc/cashu-ts](https://github.com/cashubtc/cashu-ts) — Official TypeScript Cashu wallet library. Does not yet support Spilman channels (as of v4.2.1). Our demo reimplements the channel crypto from scratch using `@noble/curves` and `@noble/hashes`, with the goal of eventually upstreaming Spilman support into cashu-ts. Strategy documented in [ADR-0005](docs/private/adr/0005-native-cashu-ts-spilman-strategy.md): three phases from hand-rolled crypto -> cdk-wasm bridge -> native cashu-ts module.

**What we copied, what we changed**:
- **Crypto functions**: Wave C delegates to cdk-wasm (same Rust crate). `crypto.js` remains for key generation, denomination splitting, and cooperative close output construction (contexts "receiver"/"sender" not yet in WASM bindings).
- **Channel lifecycle**: We hand-roll the orchestration (open → fund → pay → close) in JS. The SatsAndSports examples use `WasmSpilmanBridge`/`WasmSpilmanClientBridge` classes that manage this internally. We are **not** using the high-level bridge classes — we call individual WASM bindings and wire them together ourselves.
- **Spending conditions**: Wave C added SIG_ALL witness construction for cooperative close (P2PK 2-of-2 multisig). This is handled automatically inside the bridge classes in the reference implementation; we do it manually in `cdk-wasm-adapter.js`.
- **Mint interaction**: Standard Cashu NUT-01/02/03/05 HTTP endpoints. Same as every Cashu wallet.
- **UI**: Ours. The split-screen layout, token visualization, and signature detail panels are our design, inspired by the Option A simulator in this repo.

**Honest assessment**: Our demo is **not** architecturally aligned with the SatsAndSports reference. They use high-level bridge classes (`WasmSpilmanBridge`, `SpilmanClientBridge`) that manage the full lifecycle internally. We call low-level WASM bindings and build the orchestration ourselves. The crypto operations use the same WASM binary compiled from the same Rust source, producing identical output. But the integration pattern is different: their thin-client-over-smart-bridge vs our hand-rolled-orchestration-over-raw-bindings. This is intentional for educational purposes — our code exposes each step of the channel lifecycle explicitly.

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
