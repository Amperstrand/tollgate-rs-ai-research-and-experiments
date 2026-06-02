# Amperstrand Experimental Fork — Status Report

> This is a living document tracking the state of the Amperstrand AI research fork of [tollgate-rs](https://github.com/OpenTollGate/tollgate-rs) and [cdk](https://github.com/cashubtc/cdk). Last updated: 2026-06-02.

## TL;DR

We have a Cashu mint (cdk-mintd) running on a physical OpenWrt router, responding to real API calls, with a 17 MB RAM footprint. The tollgate-rs server binary is code-complete but has never been tested with real traffic. Everything works against mock servers. Nothing has been validated on real hardware with real network flows.

## What We Built

### tollgate-rs (this repo)

- **22.8K lines of Rust**, 339 tests passing, 0 failures
- **v1 server mode** — drop-in replacement for the Go v1 router (`tollgate-module-basic-go`), implementing all 7 API endpoints with 68 API parity tests
- **v1 client mode** — pays upstream Go v1 routers, with usage polling, auto-renewal, session recovery, and multi-gateway session management
- **Spilman channel server** — server-side handler for payment channels, tested against mock
- **NDS (NoDogSplash) valve** — integrates with captive portal for access gating
- **Browser demo** — in-browser Spilman channel lifecycle with real crypto against testnut.cashu.exchange
- **CI** — GitHub Actions builds .ipk packages for x86_64 and aarch64

### cdk (Amperstrand fork)

- **Packaged cdk-mintd as an OpenWrt .ipk** — fakewallet + sqlite, ~10 MB binary
- **CI builds both architectures** — x86_64 via musl-tools, aarch64 via native ARM GitHub runner
- **Installable and runnable** on physical OpenWrt routers

## What's Actually Proven

| Claim | Evidence |
|-------|----------|
| cdk-mintd runs on OpenWrt (aarch64) | Installed on physical router, process running, API responding |
| Mint API works end-to-end | Created mint quote, fakewallet auto-paid, state transitioned UNPAID → PAID |
| .ipk installs cleanly via opkg | Confirmed on OpenWrt 24.10.2, aarch64_cortex-a53 |
| Memory footprint is viable | 17 MB RSS, 33 MB virtual, on a 486 MB RAM router |
| Disk footprint is viable | 21.5 MB binary, 380 KB SQLite, 34 MB overlay total |
| Service management works | procd init script starts/stops correctly, auto-restart configured |
| CI produces downloadable artifacts | Both architectures build green on every push |

## What's NOT Proven

| Risk | Status |
|------|--------|
| tollgate-rs server handles real client traffic | Never tested with a real client |
| NDS valve actually gates traffic | Code written, never tested with real NoDogSplash |
| Real iptables/nftables traffic gating | Stub only — logs instead of gating (M4) |
| Compatibility with Go v1 client | 68 mock tests pass, but no real Go client has connected |
| Long-running stability | Mint has run for ~35 minutes on the router. No soak test. |
| Memory behavior under load | Unknown — only tested with single API calls |
| Lightning payments | Fakewallet only — no real LN backend tested |
| Spilman channels on router | Server handler exists but no network integration test against real mint |

## Router Hardware Details

The physical test router:

| Property | Value |
|----------|-------|
| Device | OpenWrt 24.10.2, mediatek/filogic |
| Architecture | aarch64_cortex-a53 |
| RAM | 486 MB total, ~324 MB free with mint running |
| Storage | 204 MB overlay, 165 MB free |
| Kernel | Linux 6.6.93 |
| Access | SSH at 10.171.103.1 (LAN port en6 of development laptop) |

## Architecture on Router

```
[Client] ──HTTP──> [tollgate-rs :8080] ──ecash──> [cdk-mintd :8085]
      │                    │                              │
      │              NDS valve                    fakewallet + sqlite
      │          (iptables stub)                  (auto-approves all)
      │                    │                              │
      └────────────────────┘                              │
         Traffic gated                           Tokens minted/burned
        (not yet real)                           (in-memory via /tmp)
```

Currently only the right half (cdk-mintd) is installed and running. tollgate-rs has not been deployed to the router yet.

## Known Gaps (Honest List)

1. **Traffic valve is a logging stub** — does not actually gate traffic with iptables/nftables. This is the single biggest gap for production use.
2. **No real Lightning backend** — fakewallet auto-approves everything. No CLN, LND, or LNbits integration tested.
3. **No captive portal testing** — NDS integration code exists but has never been tested with a running NoDogSplash instance.
4. **No Go v1 interop** — tollgate-rs implements the Go v1 API spec, but no Go v1 client has ever connected to it.
5. **No soak testing** — no multi-hour or multi-day stability tests on the router.
6. **No concurrent client testing** — all tests are single-client against mock servers.
7. **Unilateral close not implemented** — Spilman channels only support cooperative close.
8. **DLEQ proof verification not implemented** — in browser demo only.

## How We Got Here

| Step | What Happened |
|------|---------------|
| M1 | Core types, CBOR codec, peer state machine |
| M2 | Bootstrap token payment, CDK wallet integration |
| M2.5 | v1 client mode (pays upstream Go v1 routers) |
| M3 | Spilman payment channel server handler |
| CI | Cross-rs packaging → OpenWrt SDK → musl-tools → native ARM runner |
| Packaging | 6 failed CI iterations (openssl, protoc, glibc/musl mismatch) before finding working approach |
| Physical | .ipk installed on router, mint booted, API responding |

## Feasibility Assessment

**Running a CDK mint on an OpenWrt router is fully feasible.** The numbers speak for themselves:

- 17 MB RAM for a full Cashu mint is well within even low-end router budgets
- The cdk-mintd binary at 21.5 MB fits comfortably on 204 MB overlay
- Fakewallet + sqlite on tmpfs gives an in-memory mint with zero persistence concerns for testing
- The procd service integration works correctly with OpenWrt's init system

**Whether tollgate-rs can replace the Go v1 router in production is unproven.** The API surface matches (68 parity tests), but real-world edge cases — timing, MAC resolution quirks, NDS integration, concurrent sessions, memory leaks under load — can only be discovered by running it on real hardware with real clients.

## Next Steps

1. Install tollgate-rs .ipk on the router alongside the mint
2. Point tollgate-rs config at `http://127.0.0.1:8085/` (local mint)
3. Mint a token manually, POST it to tollgate-rs, verify session creation
4. Test against a real Go v1 client
5. Install NoDogSplash, test captive portal → payment → access flow
6. Implement real iptables/nftables valve (M4)
7. Soak test — leave it running for days, monitor memory

## Repositories

| Repo | Branch | What |
|------|--------|------|
| [Amperstrand/tollgate-rs](https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments) | `experimental` | tollgate-rs + tollgate-net |
| [Amperstrand/cdk](https://github.com/Amperstrand/cdk) | `experimentalaislop` | CDK fork with OpenWrt packaging |

## Disclaimer

This is an experimental AI-assisted research fork. It is not production software. It has not been audited. It has not been tested on real networks with real money. The code works in tests and on a single router under controlled conditions. Do not use this for anything that matters.
