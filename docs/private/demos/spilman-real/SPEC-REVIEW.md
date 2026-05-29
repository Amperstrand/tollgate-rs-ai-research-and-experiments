# Spec Review: What We Learned from Building the Demo

A retrospective on the TollGate v2 design documents (`docs/design/core/`) based on 3 weeks of experimental implementation. This document captures where the spec was prescient, where it was insufficient, where it was wrong, and where real-world constraints forced divergences. The goal is a feedback loop: use these learnings to strengthen the spec before production implementation begins.

## Terminology Consistency

The spec uses precise terms but some inconsistency across documents creates ambiguity for implementers.

### sender vs provider vs funder

The same role is called three different things:
- **tollgate-payment-channels.md**: "sender" (the party that funds and signs balance updates)
- **tollgate-protocol.md**: "sender" in message descriptions, but "funder" in rollover discussion
- **tollgate-bootstrap.md**: "provider" (the node selling delivery) and "peer" (the node buying)
- **tollgate-intro.md**: "provider" when describing access control, "sender" when describing channels

The Cashu Spilman ecosystem (SatsAndSports, CashuTube) uses "sender" and "receiver" exclusively. The demo adopted this terminology and it worked well. "Provider"/"peer" introduces confusion because in TollGate's mesh model, every node is both a provider and a peer simultaneously.

**Recommendation**: Standardize on sender/receiver for channel operations, provider/buyer for the business relationship. Never use "funder" — it's always the sender.

### peer vs node vs device

- **tollgate-intro.md**: "node" and "device" interchangeably
- **tollgate-protocol.md**: "peer" exclusively
- **tollgate-access-control.md**: "peer" for the remote party, "node" for the local system

**Recommendation**: "Peer" for the remote party in a relationship, "node" for the local system. Never "device" — that's an implementation detail.

### remaining_quota vs balance_scaled

ADR-0004 resolved this: the protocol surface uses `remaining_quota` (RFC 4006 `Granted-Service-Unit` aligned), internal code uses `balance_scaled`. This was a good decision — the bootstrap spec (tollgate-bootstrap.md) was updated to match.

### ISO/RFC Alignment

ADR-0004 deliberately aligned TollGate's exhaustion model with RFC 4006/8506 (Diameter Credit-Control). This was the right call. The mapping:

| TollGate | RFC 4006 | Adopted? |
|---|---|---|
| `remaining_quota` | `Granted-Service-Unit` | Yes |
| Terminate/Restrict/Allow | `Final-Unit-Action` | Yes |
| `is_final` | `Final-Unit-Indication` | Yes |
| `next_checkin_ms` | `Validity-Time` | Yes (more concrete name) |
| `Suspended` access level | No equivalent | Kept (clearer) |

What we deliberately did NOT adopt from RFC 4006: credit-control server, request types, quota grants, rating groups, redirect servers. TollGate is P2P, not client-server. The alignment stops at the exhaustion vocabulary.

**Recommendation**: Keep this alignment. It makes TollGate immediately comprehensible to anyone who's worked with prepaid metering systems (telecoms, utilities, cloud APIs) without importing the central-server architecture.

---

## Where the Spec Was Right

### CBOR encoding (tollgate-protocol.md)

ADR-0002 documents the migration from `ciborium` to `minicbor`. The spec's choice of CBOR was vindicated:
- Integer keys (not strings) kept messages compact — the spec's size estimates were accurate
- `no_std` compatibility via `minicbor` matters for the ESP32 target
- Self-describing format avoided custom parsers per transport

### Cumulative counters (tollgate-metering.md)

The spec says counters are "cumulative since session start, not deltas." We didn't implement metering in the demo, but the v1 Go codebase (`tollgate-module-basic-go`) uses delta counters and the cumulative approach is clearly superior. Lost reports don't corrupt accounting. The self-healing property is real.

### Take-it-or-leave-it pricing (tollgate-pricing.md)

No negotiation. Provider sets the price. Peer accepts or walks away. This was the right design. Implementing a negotiation protocol would have added enormous complexity for zero benefit in a mesh — if a peer's price is too high, route around them. The demo validated this indirectly: the lack of pricing complexity let us focus entirely on the crypto.

### Channel ownership: sender manages own lifecycle (tollgate-payment-channels.md)

The spec says "rollover is initiated by the funder alone because only the funder puts up new funds." This is correct and important. In the demo, Alice (sender) manages the entire funding flow. No coordination overhead for rollover decisions. This pattern scales well.

### "No pay, no service" (tollgate-access-control.md)

Default access level is None (blocked). This is the only safe default. The demo reinforced this — if we had started with open access, we'd have built security as an afterthought.

---

## Where the Spec Was Insufficient

### Spilman channel crypto details

The spec defines the Wallet trait with high-level operations (`fund_channel`, `sign_balance_update`, `settle_channel`). It does not specify the crypto primitives underneath. This is correct architecturally (the spec shouldn't prescribe implementation), but it leaves a gap for implementers who need to know:

- **P2BK derivation**: `SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry}")` — this comes from the SatsAndSports implementation, not the spec
- **SIG_ALL message format**: `secret_0 || C_0 || ... || secret_n || C_n || amount_0 || B_0 || ... || amount_m || B_m` — from Cashu NUT-03, not the TollGate spec
- **2-of-2 multisig witness construction**: How to build the P2PK witness for cooperative close — implementation-specific
- **Deterministic output derivation**: Secrets, blinding factors, and blinded messages are all derived from channel_secret + context — critical for auditability but not specified

These are all defined in the Cashu/NUT specs and the SatsAndSports reference, so the spec's omission isn't wrong — it's a dependency on external specifications. But an implementer needs to know where to look.

**Recommendation**: Add a "Cryptographic Dependencies" section to `tollgate-payment-channels.md` listing the NUT specs and the SatsAndSports reference as normative dependencies.

### Funding token amount calculation

The spec says "sender locks ecash in a 2-of-2 multisig" but doesn't mention that the amount to mint must account for future mint fees. The `compute_funding_token_amount` function (which calculates the exact amount to mint so that fees don't eat into the channel capacity) was something we had to discover from the SatsAndSports code. Without it, channels would be underfunded.

**Recommendation**: Add a note about fee-aware funding in the Funding section.

### DLEQ verification

The spec mentions "verifies DLEQ proofs" in the Funding step 5 but doesn't elaborate. DLEQ verification is the mechanism by which the receiver proves the mint signed with the correct key. Skipping it (as the demo does) means trusting the mint. This is a real security gap that implementers need to understand.

**Recommendation**: Expand the DLEQ note to explain the security implication of skipping it.

### Close output context names

The demo uses "receiver" and "sender" as contexts for cooperative close outputs. These context names determine the deterministic output derivation and must match what both parties compute. The spec doesn't mention these contexts at all — they're buried in the SatsAndSports implementation.

---

## Where the Spec Was Wrong or Misleading

### "Both peers can reach a mint" (Funding prerequisite)

The spec says "Both peers can reach a mint" as a prerequisite for channel establishment. In practice, this is only true for the sender (who needs to mint the funding token). The receiver doesn't need mint connectivity until settlement. The demo validated this: Charlie never contacts the mint during open/fund — only during close.

**Recommendation**: Clarify that only the sender needs mint connectivity for funding. The receiver needs it only for settlement.

### "Alice sends her private key to Charlie" (cooperative close)

The demo has Charlie hold Alice's private key for cooperative close signing. The spec implies this doesn't happen ("Receiver verifies funding proofs" — silent on key exchange). In production, Alice would sign close transactions on demand rather than sharing her key. The spec should explicitly state that cooperative close requires either:
1. Alice signs on demand (production), or
2. Alice shares a derived signing key (acceptable for demo, not for production)

### Peer state machine ambiguity

The intro describes: `new → bootstrap_received → channel_opening → active → rolling_over → settling → closed`. But the protocol defines separate state machines for each channel direction, plus an access control state machine (`None → Active → Suspended`). How these interact isn't specified. When does a peer become "active" — when the first channel is funded, or when both directions are ready?

The demo avoided this entirely by having one channel. Production needs the answer.

**Recommendation**: Define the peer-level state machine that composes the per-channel states and the access control state.

---

## Things We Learned That the Spec Doesn't Address

### WASM as a deployment target

The spec assumes Rust native or ESP32. The demo proved that Spilman channel crypto works in the browser via WASM (compiled from the same Rust crate). This opens a third deployment target: any device with a browser. The CashuTube reference implementation does the same thing.

This isn't just a demo trick. A browser-based TollGate client could enable:
- Phone browsers as TollGate payers (no app install)
- Web-based admin dashboards for operators
- Testing and education without toolchain setup

**Recommendation**: Acknowledge WASM/browser as a supported deployment target alongside native and ESP32.

### The cdk-wasm bridge pattern

The demo uses a layered approach:
1. Low-level WASM bindings (auto-generated by wasm-pack)
2. Format conversion layer (cdk-wasm-adapter.js — camelCase to snake_case JSON)
3. Orchestration layer (wallet.js — lifecycle management)

The SatsAndSports reference uses higher-level bridge classes (`WasmSpilmanBridge`, `SpilmanClientBridge`) that hide steps 1-2. Both approaches work. The spec's Wallet trait maps well to either pattern, but implementers should know both exist.

### Test vector drift

Test vectors (deterministic intermediate values captured from Rust) expire because `setup_timestamp` is part of the channel ID calculation. After 1 hour (channel TTL), the vectors become invalid for channels that would have expired. This is a testing concern the spec doesn't address.

**Recommendation**: Add a testing section noting that test vectors have a TTL and must be regenerated periodically.

### Mint fee variability

Mint fees (`input_fee_ppk`) change between keyset rotations. A channel funded under one fee regime may settle under another. The demo handles this by reading the current fee at close time, but the spec doesn't discuss this scenario.

**Recommendation**: Note that settlement fees are determined by the mint at settlement time, not at funding time.

### Binary denomination splitting

Cashu uses powers-of-2 denominations (1, 2, 4, 8, 16, 32, 64). The `maxPerOutput` parameter caps the largest single proof. Splitting 100 sat with maxPerOutput=64 gives [64, 32, 4], not [64, 36]. This is standard Cashu but wasn't obvious from the spec.

---

## Spec Architecture vs Demo Architecture

| Concern | Spec | Demo | Assessment |
|---|---|---|---|
| Crypto operations | `Wallet` trait (implementation-defined) | cdk-wasm + @noble/curves | Spec is right — trait hides crypto |
| Channel lifecycle | State machine per direction | Single channel, JS object | Demo simplified; spec's dual-channel is needed for production |
| Wire protocol | CBOR over HTTP/WebSocket | Direct function calls | Demo skipped this; spec's design is sound |
| Metering | `ResourceAdapter` trait with push streams | No metering | Spec is right — metering is resource-specific |
| Pricing | Products, scales, per-mint options | Fixed payments | Spec is right — pricing engine is independent of channels |
| Access control | 4-level enum + bloom filter | No access control | Spec is right — needed for production |
| Error handling | Reject messages with reason codes | None | Spec is right — essential for production |
| Persistence | Not specified | None | Spec needs to address this |
| Rollover | 80% threshold, overlap period | Not implemented | Spec's design is sound |
| Offline | Balance updates continue | N/A (same page) | Spec's offline model is correct |

---

## Recommendations for Spec Updates

### High Priority

1. **Add "Cryptographic Dependencies" section** to `tollgate-payment-channels.md` — list NUT specs, SatsAndSports reference, and key formulas (P2BK, SIG_ALL, deterministic output derivation)
2. **Clarify mint connectivity requirements** — sender needs mint for funding, receiver needs mint only for settlement
3. **Define peer-level state machine** — compose per-channel and access control states into a single coherent lifecycle
4. **Document fee-aware funding** — explain `compute_funding_token_amount` or equivalent

### Medium Priority

5. **Standardize terminology** — sender/receiver for channels, provider/buyer for business, peer/node for parties
6. **Acknowledge WASM/browser** as a deployment target
7. **Add testing section** — test vector TTL, drift prevention, oracle pattern
8. **Expand DLEQ note** — explain security implications of skipping verification
9. **Document close key requirements** — cooperative close needs either on-demand signing or derived key sharing

### Low Priority

10. **Note settlement fee variability** — fees at close time may differ from funding time
11. **Add denomination splitting reference** — powers-of-2 is standard Cashu, not obvious to newcomers
12. **Reference ADR-0004 RFC 4006 alignment** — bootstrap spec is already updated, but a cross-reference in the intro would help

---

## What to Keep As-Is

- CBOR encoding choice and integer field keys
- Cumulative counter model for metering
- Take-it-or-leave-it pricing
- Sender-manages-own-channel rollover model
- 4-level access control enum
- "No pay, no service" default
- Interval netting (only net debtor signs)
- Dual pricing dimensions (time + units)
- Bootstrap token verification requirement
- Zero-price shortcut
- The Wallet and ResourceAdapter trait boundaries
