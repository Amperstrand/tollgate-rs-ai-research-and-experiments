# ADR-0002: Switch from ciborium to minicbor for Wire Protocol

- **Status**: Accepted
- **Date**: 2026-05-06
- **Deciders**: Project owner, Sisyphus
- **Supersedes**: ADR-0001 (partially — updates library choice, keeps CDDL-as-doc approach)

## Context

ADR-0001 chose ciborium/serde as the CBOR implementation. During M1.4 research, we discovered that **ciborium + serde cannot produce integer CBOR map keys**. The `#[serde(rename = "0")]` attribute generates text string keys (`"0"`, `"1"`) instead of integer keys (`0`, `1`) as required by the CDDL spec and the protocol design.

Verified empirically — Disconnect message bytes:

```
ciborium produces: a2 61 30 62 31 34 61 31 09  (TEXT keys "0", "14", "1")
CDDL requires:     a2 00 0e 01 09              (INTEGER keys 0, 14, 1)
```

## Decision

We switch from **ciborium + serde** to **minicbor** for all wire protocol encoding/decoding.

minicbor provides native integer map key support via `#[n(0)]` attributes on struct fields with `#[cbor(map)]` on the struct itself.

## Rationale

### ciborium + custom serde — rejected

| Aspect | Assessment |
|--------|------------|
| Integer keys | Requires custom `Serialize`/`Deserialize` for all 15 types (~75 field pairs) |
| no_std | ciborium requires `std` — incompatible with ESP32 target (M6) |
| Maintenance | Custom serde impls are fragile and verbose |
| Ecosystem | CDK uses ciborium, but we don't need CDK for wire encoding |

### minicbor — accepted

| Aspect | Assessment |
|--------|------------|
| Integer keys | `#[n(0)]` attribute — built-in, zero boilerplate |
| no_std | Core feature — compatible with ESP32 (M6) |
| Forward compat | Unknown fields ignored during decode (protocol evolution) |
| Production use | Used by radicle-link, embedded firmware projects, real CBOR protocols |
| Derive macros | `#[derive(Encode, Decode)]` with `#[cbor(map)]` — declarative |
| Size | Lightweight, suitable for constrained devices |

### Other CBOR crates considered

| Crate | Integer keys | no_std | Assessment |
|-------|-------------|--------|------------|
| ciborium + serde | Custom impl needed | No | Rejected |
| minicbor | Native (`#[n()]`) | Yes | **Accepted** |
| cbor4iiot | Via serde only | Yes | Same serde problem as ciborium |
| serde_cbor | Via serde only | No | Deprecated |
| rust-cbor | Via serde only | No | Abandoned |

### Why not stay with ciborium for CDK compatibility?

CDK (cashubtc/cdk) uses ciborium for its internal CBOR needs. However:
1. Our wire protocol types are independent of CDK's CBOR types
2. The `Wallet` trait (M1.3) abstracts CDK away — implementations handle the translation
3. We need integer keys for protocol correctness, which ciborium+serde fundamentally cannot provide without custom code
4. ESP32 support (a hard requirement) rules out ciborium

## How Each Message Type Maps to minicbor

```rust
#[derive(minicbor::Encode, minicbor::Decode)]
#[cbor(map)]
struct Announce {
    #[n(0)] protocol_version: u8,
    #[n(1)] pubkey: [u8; 33],
    #[n(2)] unit: String,
    #[n(3)] capabilities: u32,
}
```

- `#[cbor(map)]` — encode as CBOR map (not array)
- `#[n(K)]` — field at integer key K in the map
- `Option<T>` fields with value `None` are omitted from the map (matches CDDL `?` syntax)
- Fixed-size byte arrays (`[u8; 33]`, `[u8; 32]`, `[u8; 64]`) use `#[cbor(with = "minicbor::bytes")]` or custom Encode/Decode

### Message enum dispatch

The `Message` enum uses a custom `Decode` impl that:
1. Probes the CBOR map for integer key 0 (the type discriminator)
2. Rewinds the decoder
3. Dispatches to the appropriate struct's `Decode` impl

`Encode` delegates directly to the inner struct's `Encode` impl.

## Consequences

- **ADR-0001 partially updated**: CDDL-as-doc approach remains. Library choice changes from ciborium to minicbor.
- **Cargo.toml**: Replace `ciborium` with `minicbor` (both `minicbor` and `minicbor-derive`)
- **protocol.rs**: Rewrite all 15 message types with `#[derive(Encode, Decode)]` + `#[cbor(map)]` + `#[n(K)]`
- **Tests**: Use `minicbor::to_vec` / `minicbor::decode` instead of `ciborium::into_writer` / `ciborium::from_reader`
- **Domain types (M1.3)**: Unaffected — they never had serde/CBOR derives
- **Framing helpers**: Use `minicbor::to_vec` for encoding, custom parser for 2-byte LE frames
- **CI**: No changes needed — `cargo test` runs the same way

### Projects using ciborium that we're diverging from

- **CDK (cashubtc/cdk)**: Uses ciborium for Cashu token encoding. Our Wallet trait abstracts this away.
- **ciborium ecosystem**: Well-tested but serde-first, which is the wrong abstraction for integer-key CBOR maps.

### Projects using minicbor that validate our choice

- **radicle-link (radicle-dev/radicle-link)**: P2P protocol with minicbor, `#[cbor(map)]` + `#[n()]` pattern
- **portal-software (TwentyTwoHW/portal-software)**: Embedded firmware, minicbor for CBOR protocol
- **Oura (txpipe/oura)**: Cardano blockchain indexer, uses minicbor for CBOR-heavy pipeline
