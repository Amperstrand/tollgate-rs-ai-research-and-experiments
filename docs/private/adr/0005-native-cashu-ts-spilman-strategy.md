# ADR-0005: Native cashu-ts Spilman Strategy with cdk-wasm Oracle

- **Status**: Accepted
- **Date**: 2026-05-11
- **Deciders**: Project owner, Hephaestus
- **Related**: ADR-0003 (CDK for Cashu ops), `cdk_spilman_test_vectors.rs`

## Context

The in-browser Spilman demo (`docs/private/demos/spilman-real/`) proves that Spilman channel crypto works in JavaScript using `@noble/curves` and `@noble/hashes`. It currently hand-rolls 16 crypto functions (551 lines in `crypto.js`) translated from the Rust `cdk-spilman` crate. These functions are production-grade in algorithm but have known simplifications that make them unsuitable for direct upstreaming to cashu-ts:

1. **Simplified message hash**: signs `SHA256(channel_id || "|" || balance)` instead of the full `sig_all_message_hash` (serialized swap request with commitment outputs)
2. **No DLEQ verification**: mint blind signatures trusted without proof
3. **No unilateral/timeout close**: cooperative close only
4. **No persistence**: in-memory only
5. **Single-page architecture**: both wallets in same JS context, no real peer separation

The long-run goal is **native Spilman channels in cashu-ts** - the official TypeScript Cashu library. This repo cannot implement that directly (cashu-ts is a separate repo, cashubtc/cashu-ts), but we can:
- Build and validate the implementation here in our demo
- Use `cdk-wasm` (compiled from `cdk` + `cdk-spilman`) as a runtime bridge and test oracle
- Maintain byte-for-byte equivalence with Rust via shared test vectors
- Prepare a clean, upstreamable PR when ready

## Decision

### Three-Phase Strategy

```
Phase 0 (Now)       Phase 1 (Bridge)         Phase 2 (Native)
[ Hand-rolled ]    [ cdk-wasm bridge ]     [ cashu-ts Spilman ]
[ crypto.js   ] -> [ oracle + fallback] -> [ module upstreamed]
[ @noble/*    ]    [ demo + CI        ]    [ @noble/* deps    ]
     Demo              Demo + CI              cashu-ts repo
```

**Phase 0 (Current)**: Hand-rolled `crypto.js` - educational, proves feasibility, has known gaps.

**Phase 1 (Bridge)**: Load `cdk-wasm` alongside the demo. cdk-wasm runs real Rust crypto in-browser via WASM. Our JS crypto functions remain but are validated against cdk-wasm output. cdk-wasm serves as oracle for CI drift detection.

**Phase 2 (Native)**: Contribute Spilman channel module to cashu-ts. Uses same `@noble/curves`/`@noble/hashes` deps that cashu-ts already depends on. Our demo switches from hand-rolled crypto to the cashu-ts module. cdk-wasm remains as CI oracle.

### cdk-wasm Role

cdk-wasm is **not** the destination. It is the bridge and the oracle:

| Role | When | How |
|------|------|-----|
| **Runtime bridge** | Phase 1 | cdk-wasm handles operations our JS crypto can't yet (full sig_all_message_hash, DLEQ, unilateral close) while we implement them in TS |
| **CI oracle** | All phases | Test harness calls both cdk-wasm and our TS implementation on identical inputs, asserts byte-for-byte output equivalence |
| **Protocol reference** | All phases | cdk-wasm output defines "correct" - our TS implementation must match exactly |
| **Discarded** | Phase 2+ | Once cashu-ts has native Spilman, cdk-wasm is no longer needed at runtime. It may remain in CI as oracle for as long as useful |

### Drift Prevention Mechanisms

Drift between our TS implementation and the Rust reference (cdk-spilman) is the primary risk. Prevention is explicit:

1. **Shared test vectors**: `cdk_spilman_test_vectors.rs` captures deterministic intermediate values to `test-vectors.json`. JS-side tests load this file and assert exact match. See `TEST-VECTORS.md` in the demo directory for the contract.

2. **Byte-for-byte assertions**: Every crypto function in `crypto.js` must produce identical hex output to the corresponding Rust function, given identical inputs. Not "approximately equal" - exact hex match.

3. **Protocol version string**: The domain separation prefixes (`"Cashu_Spilman_channel_secret_v1"`, `"Cashu_Spilman_P2BK_v1"`, `"Secp256k1_HashToCurve_Cashu_"`) are shared constants. Any version bump in cdk-spilman must be reflected in our TS implementation simultaneously.

4. **cdk-wasm CI oracle**: A test harness (Playwright or Node) that:
   - Generates random channel parameters
   - Calls cdk-wasm functions and our TS functions with identical inputs
   - Asserts output equivalence on: channel_id, blinded messages, unblinded proofs, Schnorr signatures
   - Runs on every PR that touches `crypto.js` or `wallet.js`

5. **Known-simplification tracking**: Every gap between our JS and Rust is enumerated (see Gap Table below). Each gap has a status and a target phase for resolution.

### Gap Table: JS vs Rust

| # | Gap | Impact | Status | Target |
|---|-----|--------|--------|--------|
| G1 | Simplified message hash (`SHA256(id\|balance)` vs `sig_all_message_hash`) | Balance update signatures not interoperable with Rust peers | Open | Phase 1 (cdk-wasm bridge) |
| G2 | No DLEQ verification of mint blind signatures | Trusts mint not to exploit blinding | Open | Phase 2 (cashu-ts native) |
| G3 | Cooperative close only (no unilateral/timeout) | No recourse if counterparty disappears | Open | Phase 2 (cashu-ts native) |
| G4 | No persistence | Channel state lost on page reload | Open | Phase 1 (IndexedDB) |
| G5 | Single-page wallet architecture | No real peer-to-peer separation | Open | Phase 1 (iframe/worker) |
| G6 | No revocation logic | Cannot invalidate old balance updates | Open | Phase 2 (cashu-ts native) |
| G7 | Oversized mint quote (waste) | Excess tokens not reclaimed | Open | Phase 1 (exact amount) |
| G8 | No multi-channel support | One channel per page load | Open | Phase 2 (cashu-ts native) |

### cashu-ts Upstreaming Prerequisites

Before proposing Spilman to cashu-ts, these must be ready:

1. **Complete crypto parity**: All 16+ functions produce identical output to cdk-spilman, verified by shared test vectors + cdk-wasm oracle
2. **Full close paths**: Cooperative + unilateral + timeout
3. **DLEQ verification**: Must match cdk DLEQ implementation
4. **sig_all_message_hash**: Must compute the full serialized swap request hash
5. **Channel state machine**: Production-quality lifecycle (open -> fund -> pay -> close in all modes)
6. **Existing test infrastructure**: cashu-ts already uses `@noble/curves` and `@noble/hashes` - our code should be a natural fit
7. **NUT spec alignment**: Spilman channel spec must be finalized or near-final in the NUTs repository

## Consequences

- **Positive**: Clear roadmap from hand-rolled demo to production-grade upstream contribution
- **Positive**: cdk-wasm provides immediate bridge for operations we can't yet implement in TS
- **Positive**: Drift prevention is structural (shared vectors, CI oracle), not ad-hoc
- **Positive**: cashu-ts already uses `@noble/curves` - no new cryptographic dependency
- **Negative**: cdk-wasm is ~1.74 MiB WASM - significant for browser demo (Phase 1 only)
- **Negative**: cdk-wasm is not published on npm - must host JS+WASM directly (CashuTube pattern)
- **Negative**: Upstreaming to cashu-ts requires coordination with cashubtc maintainers - timeline not under our control
- **Risk**: cdk-spilman API may change between versions - test vectors must be regenerated

## References

- ADR-0003: CDK chosen for Rust Cashu operations
- `cdk_spilman_test_vectors.rs`: Rust test vector capture
- `crypto.js`: 16 JS crypto functions, 1:1 mapping to cdk-spilman
- [cashubtc/cashu-ts](https://github.com/cashubtc/cashu-ts): Target upstream
- [SatsAndSports/cashu_spilman_channels](https://github.com/SatsAndSports/cashu_spilman_channels): cdk-spilman source
- [cashubtc/cdk](https://github.com/cashubtc/cdk): cdk-wasm source
