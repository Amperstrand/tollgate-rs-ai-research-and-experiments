# AGENTS.md — Amperstrand Private Fork

This is the **private Amperstrand fork** of [OpenTollGate/tollgate-rs](https://github.com/OpenTollGate/tollgate-rs).

## CRITICAL: Upstream Boundary

**This repository is a private fork of a public open-source project. The upstream project is maintained by other people.**

### ABSOLUTE RULES

1. **NEVER push to `OpenTollGate/tollgate-rs` (the upstream `origin` remote).** No direct commits, no force pushes, no branch creation, no issue comments, no PRs, nothing. The upstream remote exists for pulling upstream changes only.

2. **NEVER open issues or PRs on the upstream repo.** If upstream contributions are desired, they will be carefully prepared and submitted by the project owner through a deliberate, reviewed process — not by an automated agent.

3. **NEVER interact with the upstream repo's GitHub in any way** — no comments, no reactions, no issue creation, no discussions, no wiki edits. Zero. The upstream maintainers should never see activity from this fork.

4. **All work happens on the `private` remote** (`https://github.com/Amperstrand/tollgate-rs.git`). Branch from `master`, push to `private`, create PRs against `private`.

5. **When syncing from upstream**, always `git fetch origin` and merge/rebase into local branches. Never push those merged changes back to `origin`.

### Git Remote Configuration

```
origin  → https://github.com/OpenTollGate/tollgate-rs    (READ-ONLY upstream, fetch only)
private → https://github.com/Amperstrand/tollgate-rs.git (READ-WRITE, this fork)
```

- `git pull origin master` — OK (sync from upstream)
- `git push private feature-branch` — OK (work on our fork)
- `git push origin anything` — **FORBIDDEN**
- `gh issue create --repo OpenTollGate/tollgate-rs` — **FORBIDDEN**
- `gh pr create --repo OpenTollGate/tollgate-rs` — **FORBIDDEN**

## Project Overview

TollGate v2 — Rust implementation of the TollGate payment protocol for autonomous, device-to-device payment for metered resource delivery. Built on Cashu ecash and Spilman payment channels.

The upstream repo is in the **design phase** — detailed design documents exist under `docs/design/` but no Rust code has been written yet. This fork will contain the implementation work.

### Architecture

```
tollgate-core (lib)           Pure logic, resource-agnostic
    |
    +-- tollgate-net (binary) Network forwarding, feature-flagged per OS
    |     +- Linux / macOS / Windows / OpenWrt
    |     +- FIPS or IP network adapter
    |     +- Cashu wallet
    |
    +-- tollgate-net-esp32 (separate project, not in this repo)
```

### Key Design Documents

Start with `docs/design/core/tollgate-intro.md`, then follow the reading order in `docs/design/README.md`.

## Milestone Plan

| Milestone | Description | Status |
|-----------|-------------|--------|
| M1 | Core Types, Protocol Codec, and Peer State Machine | Open |
| M2 | Bootstrap Token Payment (v1-Parity Payment Mode) | Open |
| M2.5 | v1 Client Mode (tollgate-rs pays v1 routers) | Open |
| M3 | Spilman Payment Channels | Open |
| M4 | tollgate-net — IP Peering Deployment | Open |
| M5 | Dynamic Pricing and Operator Controls | Open |
| M6 | FIPS Mesh Integration | Open |
| M7 | Production Hardening and Packaging | Open |

### Dependency Graph

```
M1 (Core Types/Codec)
 |
 +--> M2 (Bootstrap Tokens) --> M3 (Spilman Channels) --> M6 (FIPS Mesh)
 |        |                        |
 |        +------------------------+--> M4 (IP Peering Deployment)
 |                                     |
 |                                     +--> M5 (Dynamic Pricing)
 |
 +--> M7 (Production) depends on M4

M2.5 (v1 Client) branches off M2, can proceed in parallel with M3
```

### Critical Path

M1 → M2 → M3 → M4 → M7

### M2 is the First Working TollGate

M2 (bootstrap tokens only, no Spilman) gives a functional TollGate with token-based payments. This is feature-equivalent to v1's payment model but running on v2's architecture. If Spilman channels (M3) prove too hard, M2 + M4 is a shippable product.

## Working Conventions

### Branching

- Branch from `master` on the `private` remote
- Branch naming: `m{number}/{short-description}` (e.g., `m1/cbor-codec`, `m2/bootstrap-tokens`)
- Push branches to `private` only

### Implementation Notes

- Design documents in `docs/design/` are the source of truth for protocol behavior
- If implementation reveals issues with the design, document them in `docs/private/` (not in the design docs — those track upstream)
- Architecture Decision Records go in `docs/private/adr/`
- Experiment results, library evaluations, and dead-end notes go in `docs/private/notes/`

### Cashu / Wallet

- Starting with simple Cashu token operations (receive, verify, create) for M2
- Spilman channel wallet operations deferred to M3
- Library choice TBD — evaluate `cdk` Rust crate and alternatives during M2

### Testing

- Every milestone must have integration tests, not just unit tests
- M2 must include a two-node localhost integration test
- M3 must include multi-interval channel lifecycle tests
- M4 must include tests against real network traffic (even if loopback)

## Related Projects

- [tollgate-module-basic-go](https://github.com/OpenTollGate/tollgate-module-basic-go) — TollGate v1 (Go, OpenWrt). The established production implementation. **Read-only reference. Do not interact with this repo.**
- [FIPS](https://github.com/nicobao/fips) — Free Internetworking Peering System. Target mesh network substrate for M6.
- [Cashu](https://cashu.space/) — Ecash protocol used for payments.
