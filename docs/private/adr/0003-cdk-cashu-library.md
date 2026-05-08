# ADR-0003: Use CDK for Cashu Wallet Operations

- **Status**: Accepted
- **Date**: 2026-05-06
- **Deciders**: Project owner, Sisyphus

## Context

M2 requires a Rust Cashu library for bootstrap token operations: decode Cashu tokens, verify proofs with a mint, create tokens, check balances. M3 will need Spilman channel operations.

## Decision

Use **`cdk`** (cashubtc/cdk) as the Cashu library for `tollgate-core`.

## Evaluation

| Library | Version | Async | HTTP Mint | Token V3/V4 | Spilman | Maturity |
|---------|---------|-------|-----------|-------------|---------|----------|
| **cdk** | 0.16.0 | ✅ | ✅ (HttpClient) | ✅ cashuA + cashuB | ❌ (not yet) | Active, maintained |
| cashu-rs | 0.0.2 | ❌ sync | ❌ (request builders only) | V3 only | ❌ | WIP |
| moksha-wallet | 0.4.1-alpha | ✅ | ✅ | Partial | ❌ | Early dev |
| cashu-crab | 0.0.1 | ✅ | Partial | Partial | ❌ | Alpha |

## Rationale

- **cdk is the only production-viable option**: async API, built-in HTTP mint client (reqwest-based), supports both V3 (`cashuA`) and V4 (`cashuB`) token formats
- **MSRV 1.85.0** — matches our MSRV exactly
- **Active maintenance**: part of the official cashubtc GitHub org, regularly updated
- **Token operations**: `receive()` decodes + verifies tokens, `send` saga creates tokens, `check_proofs_spent()` validates with mint via `/v1/checkstate`
- **Balance tracking**: `total_balance()` over unspent proofs, per-mint

### Spilman Channels (M3)

We use **[SatsAndSports/cashu_spilman_channels](https://github.com/SatsAndSports/cashu_spilman_channels)** (`cdk-spilman` crate) for Spilman channel crypto primitives: proof construction, keyset parsing, and DLEQ operations.

The channel orchestration (HTTP calls to mint, quote polling, balance update signing, cooperative close flow) is our own code in `SpilmanChannelManager` (`spilman_wallet.rs`). It calls `cdk-spilman` for the cryptographic operations and handles the HTTP/network layer itself.

### Token Format Notes

- `cashuA...` = base64url(JSON) — V3 format, widely supported
- `cashuB...` = base64url(CBOR) — V4 format, compact keys
- CDK rejects multi-mint V3 tokens in `receive()`
- No need to support V1/V2 token formats — no current parsers support them

### Integration Approach

For M2 (bootstrap tokens only):
- `Wallet::receive_token()` → CDK `Wallet::receive()` — decode + verify + credit
- `Wallet::create_token()` → CDK send saga — create proofs + encode token
- `Wallet::mint_reachable()` → CDK `HttpClient::get_mint_info()` — health check
- `Wallet::balance()` → CDK `Wallet::total_balance()` — aggregate unspent

CDK wallet is `Arc<Mutex<Wallet>>` internally, so thread-safe.

## Consequences

- **Positive**: Production-quality Cashu operations, async HTTP, active community
- **Positive**: MSRV compatible, no custom crypto implementations
- **Negative**: No Spilman support — must implement ourselves or wait
- **Negative**: CDK is still pre-1.0 — API may change between minor versions
- **Risk**: If CDK abandons Spilman plans, we're implementing from scratch anyway

## References

- CDK repository: https://github.com/cashubtc/cdk
- CDK on crates.io: https://crates.io/crates/cdk
- Cashu token format: https://github.com/cashubtc/cashu-ts/blob/main/src/model/types/token.ts
- Cashu Spilman reference: https://github.com/cashubtc/cashu-spilman-channels
