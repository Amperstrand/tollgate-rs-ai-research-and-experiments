# ADR-0001: CDDL as Documentation, ciborium as Implementation

- **Status**: Accepted
- **Date**: 2026-05-06
- **Deciders**: Project owner, Sisyphus
- **Related**: Issue #3 (M1.1), Issue #1 (M1.4 Codec)

## Context

TollGate v1 uses CBOR (RFC 8949) as its wire format. We need to decide how to define and implement the protocol schema for 15 message types with integer map keys, optional/nullable fields, and byte-size constraints.

Two approaches were evaluated:

1. **CDDL as source of truth** — write `protocol/tollgate.cddl`, generate or validate Rust types from it
2. **ciborium/serde as source of truth** — define Rust structs with serde derives, CDDL as documentation

## Decision

We use **ciborium/serde as the implementation source of truth**. The CDDL schema at `protocol/tollgate.cddl` remains as canonical human-readable documentation for cross-language implementers.

## Rationale

### CDDL as source of truth — rejected

| Pro | Con |
|-----|-----|
| Cross-language by design (zcbor → C) | No mature Rust codegen from CDDL exists |
| Machine-verifiable via `cddl` CLI | CDDL tooling is fragile (timed out / failed to build) |
| Single canonical protocol document | Must maintain CDDL + Rust structs in sync manually |
| IETF standard (RFC 8610) | CDK (Cashu library) uses ciborium directly, no CDDL |
| | No Cashu ecosystem project uses CDDL |

### ciborium/serde as source of truth — accepted

| Pro | Con |
|-----|-----|
| Single source of truth (Rust structs) | Rust-specific; other languages have no machine-readable schema |
| Serde ecosystem is mature and well-tested | Protocol spec lives in `.rs` files, not standalone document |
| Natural interop with CDK (both use ciborium) | Serde integer-key attributes are verbose |
| Round-trip tests catch any encoding issues | |
| Every production Rust CBOR project works this way | |

### Drift management

- The CDDL file (270 lines) maps 1:1 to Rust message structs
- CI round-trip tests (encode struct → CBOR bytes → decode → assert equal) catch drift immediately
- No generation tooling in either direction — both files are maintained, tests verify consistency
- The CDDL is simple enough that manual sync is trivial for 15 message types

### Research findings that informed this decision

- **CDK (cashubtc/cdk)**: uses `ciborium` + `cbor-diag`, no CDDL files anywhere in Cashu ecosystem
- **FIPS (jmcorgan/fips)**: custom binary + handwritten Rust codecs, no schema language
- **Ockam (build-trust/ockam)**: Rust project with CDDL for validation docs, serde for implementation
- **Production CDDL precedents**: COSE, Cardano, Nordic nRF — all use CDDL as spec, native types as implementation

## Consequences

- `protocol/tollgate.cddl` is documentation, not a build artifact
- Rust message structs with `#[serde(rename_all = "0")]` style attributes define the wire format
- CI must include round-trip CBOR tests for all 15 message types
- When adding new message types: update CDDL docs, update Rust structs, add test
- Cross-language implementers reference CDDL, build their own native types from it
- CBOR library choice is ciborium (consistent with CDK dependency)
