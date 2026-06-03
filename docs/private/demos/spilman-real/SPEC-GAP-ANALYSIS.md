# Spec Gap Analysis: Demo vs Design Documents

Comparison of the M3 Spilman channel demo (`docs/private/demos/spilman-real/`) against the design specifications in `docs/design/core/`. This is a snapshot of what we learned from building the demo — what we proved works, what we skipped, and what a production implementation must add.

## What the Demo Proves Works

These spec concepts are implemented and tested against a real mint:

| Spec concept | Demo implementation | Source |
|---|---|---|
| ECDH channel secret | `compute_channel_secret(privKey, theirPubKey)` via WASM | wallet.js |
| 2-of-2 P2PK multisig funding token | `create_funding_outputs` with NUT-10/11 spending conditions | wallet.js |
| P2BK deterministic blinded outputs | `deriveBlindingScalar` with retry loop until valid scalar | cdk-wasm-adapter.js |
| Channel ID = SHA-256(params + channel_secret) | `channel_parameters_get_channel_id` via WASM | wallet.js |
| Sender signs balance updates | `spilman_channel_sender_create_signed_balance_update` via WASM | wallet.js |
| SIG_ALL witness (atomic input/output commitment) | `computeSigAllMessage` serializes all inputs + outputs | cdk-wasm-adapter.js |
| Cooperative close via mint swap | `POST /v1/swap` with 2-party witness | wallet.js |
| Only latest commitment matters | `applyPayment` replaces previous state | channel.js |
| Mint fees deducted from sender's refund | `ceil(inputTotal * inputFeePpk / 1000)` | wallet.js |
| Proof construction via blind signature unblinding | `construct_proofs` via WASM | wallet.js |
| Funding token amount accounts for mint fees | `compute_funding_token_amount(capacity, keysetInfo, maxPerOutput)` | wallet.js |

The crypto is correct — test vectors validated against Rust, 6/6 E2E tests passing against testnut.cashu.exchange, live demo verified on GitHub Pages.

---

## What the Spec Describes That the Demo Does Not Implement

### 1. Bidirectional Channel Pairs

**Spec** (tollgate-payment-channels.md): "Each pair of TollGate peers maintains two unidirectional Spilman channels — one per delivery direction." Each peer funds their own channel.

**Demo**: One channel only. Alice is always sender, Charlie is always receiver. No channel from Charlie to Alice.

**Why**: The demo is educational — showing one direction is sufficient to demonstrate the crypto. Bidirectional requires two parallel state machines and is a deployment concern, not a crypto concern.

### 2. Interval Netting

**Spec** (tollgate-payment-channels.md): Both peers compute `net = A_owes_B - B_owes_A`. Only the net debtor signs a balance update. Zero net = no update. This dramatically extends channel life when peers have similar resource flow.

**Demo**: Alice pays Charlie directly. No metering, no pricing, no netting.

**Why**: Netting requires metering data (bytes delivered/received) and pricing, which the demo doesn't model.

### 3. Channel Rollover

**Spec** (tollgate-payment-channels.md): At 80% capacity, sender initiates `RolloverInit` with new channel funding. Old channel drains to 100%, new channel takes over. Both active simultaneously during overlap period. Example: old channel has 2 sat remaining, interval costs 5 sat — 2 goes to old, 3 goes to new.

**Demo**: Single channel, no rollover.

**Why**: Rollover requires two simultaneous channels for the same direction and split-interval billing. Significant orchestration complexity that doesn't teach new crypto concepts.

### 4. Unilateral Close

**Spec** (tollgate-payment-channels.md): "Receiver unilateral close — receiver acts without sender cooperation, presents the latest signed balance update and cannot claim more than that cumulative amount."

**Demo**: ✅ Implemented. Charlie can close without Alice's cooperation using the same swap mechanics with `validate_due=false` per Rust bridge.rs:1681-1689. Both cooperative and unilateral close buttons available in the UI.

### 5. Timeout Refund

**Spec** (tollgate-payment-channels.md): "Sender timeout refund — sender waits until funding-token expiry path is valid, recovers funds not claimed by a valid receiver settlement."

**Demo**: No timeout path exercised. The expiry timestamp is set (now + 3600s) but the demo never triggers it.

**Why**: The refund path requires `get_sender_blinded_pubkey_for_stage1_refund` (params.rs) which isn't yet exposed in the WASM bindings. The spending condition supports it but the witness construction for timeout refund isn't implemented.

### 6. Metering

**Spec** (tollgate-protocol.md): `MeteringReport` messages with cumulative delivered/received counters, elapsed time. Both sides send at each interval. Counters reset at session start. Cumulative values make the protocol self-healing (lost/duplicated reports don't corrupt accounting).

**Demo**: ✅ Interactive utility meter implemented. Charlie sells electricity at 5 watts, 1 sat/watt-second. When Alice turns on the light bulb, the meter ticks down her balance at 5 sat/sec with auto-payments through the channel. This is a visual demo of metered resource consumption — not the full `MeteringReport` protocol (no cumulative counters, no dual pricing, no interval-based reports).

**Why**: The meter demonstrates the *concept* of pay-per-use resource delivery. The full metering protocol (cumulative counters, elapsed time, dual pricing dimensions) would require a resource adapter and the `MeteringReport` message format.

### 7. Pricing / PriceSheet

**Spec** (tollgate-protocol.md): `PriceSheet` with products, mint options, `price_per_second` and `price_per_unit`. Dual pricing dimensions (time + units). Product IDs are SHA-256 hashes. Each side sends their own sheet.

**Demo**: No pricing. Fixed payments of 10 sat + 20 sat.

**Why**: Pricing requires metering data and a product model. The demo focuses on channel crypto, not the billing layer.

### 8. Wire Protocol (CBOR Messages)

**Spec** (tollgate-protocol.md): 15 message types encoded in CBOR over HTTP polling (`POST /tollgate/v1/exchange`) or WebSocket (`GET /tollgate/v1/ws`). Field keys are small integers. Messages: Announce, PriceSheet, Accept, ChannelReady, MeteringReport, BalanceUpdate, BalanceAck, BootstrapToken, BootstrapAck, RolloverInit, RolloverReady, ChannelClose, CloseAck, Reject, Disconnect.

**Demo**: Direct JavaScript function calls between Alice and Charlie objects in the same page. No wire protocol, no CBOR, no transport.

**Why**: The wire protocol is a deployment concern (tollgate-net, M4). The demo proves the crypto; the wire protocol can be added independently. M1 already implements the CBOR codec in Rust.

### 9. Announce / Negotiation

**Spec** (tollgate-protocol.md): Both peers send Announce (version, pubkey, capabilities bitfield). SPILMAN capability bit signals channel support. Version mismatch = Reject. Then PriceSheet exchange, then Accept with channel funding.

**Demo**: Alice creates a wallet, gets Charlie's pubkey from the UI. No negotiation, no version check, no capability bits.

### 10. BalanceAck

**Spec** (tollgate-protocol.md): After BalanceUpdate, the creditor sends `BalanceAck` confirming the accepted cumulative balance.

**Demo**: Charlie accepts payments silently. No acknowledgment.

### 11. DLEQ Proof Verification

**Spec** (tollgate-payment-channels.md, Funding step 5): "Receiver verifies: re-derives blinded messages, verifies DLEQ proofs, checks mint/keyset policy."

**Demo**: `construct_proofs` parses DLEQ proofs from mint signatures but does not verify them. Trusts the mint is signing with the correct key.

**Impact**: Without DLEQ, a malicious mint could sign with a different key than it published, making the proofs unspendable by the intended recipient. Production implementations must verify.

### 12. ChannelReady Message

**Spec** (tollgate-protocol.md): After verifying funding, receiver sends ChannelReady. Resource metering begins when both channels are ready.

**Demo**: No ChannelReady. Funding is immediately active.

### 13. Bootstrap Tokens

**Spec** (tollgate-bootstrap.md): BootstrapToken, BootstrapAck messages. Bootstrap-only mode for constrained devices. Token verification with mint. Balance tracking at scaled precision (milli-sats). Exhaustion actions (Terminate, Restrict, Allow). Adaptive check-in interval based on spend rate.

**Demo**: No bootstrap tokens. Alice mints fresh tokens from the mint directly via Lightning invoice.

### 14. Disconnect / Reject / Error Handling

**Spec**: Disconnect for orderly teardown. Reject with reason codes (price too high, mint not accepted, unit not accepted, balance verification failed, transit loss exceeded, etc.). Error handling for funding failure, settlement failure, keyset errors.

**Demo**: No error recovery. If something fails, the user hits Reset.

### 15. State Persistence / Recovery

**Spec** (tollgate-payment-channels.md): Friendly recovery via ChannelSync message. Unfriendly outcome: rebooted peer loses incoming channel earnings. Exposure bounded by channel capacity and TTL.

**Demo**: In-memory only. Page reload = total state loss.

### 16. Adaptive Capacity Growth

**Spec** (tollgate-payment-channels.md): Channel capacity starts small (100 sats), grows with relationship (200 → 500 → 1000 after successful rollovers). Growth curve operator-configurable.

**Demo**: Fixed 100 sat capacity.

### 17. Price Renegotiation

**Spec** (tollgate-protocol.md): Provider piggybacks new product_id and pricing on MeteringReport (fields 4-5). Peer accepts by continuing or rejects with ChannelClose.

**Demo**: No dynamic pricing.

### 18. Zero-Price Shortcut

**Spec** (tollgate-protocol.md): When both prices are zero, skip funding entirely. No channels, no metering.

**Demo**: Not applicable (no pricing model).

---

## What the Demo Has That the Spec Doesn't Mention

| Demo feature | Spec reference |
|---|---|
| In-browser WASM crypto (cdk-wasm) | Not specified — spec defines Wallet trait, implementation chooses how |
| Educational UI with split-screen panels | Not specified — spec is protocol-level |
| Step-by-step lifecycle control | Not specified — spec shows normal connection sequence |
| Commitment visualization (superseded commitments) | Not specified — implied by "only latest matters" |
| Test vector validation (194 checks) | Not specified — quality assurance |
| P2BK blinding scalar derivation (JS mirrors Rust) | Not specified at this level — implementation detail |
| SIG_ALL message construction (JS mirrors Cashu nut03.rs) | Not specified at this level — implementation detail |

---

## Implications for Production (M4+)

The gaps fall into three categories:

**Crypto gaps** (block settlement correctness):
- DLEQ verification — ✅ funding verified (4/4 proofs), settlement verification still needed
- Unilateral close witness — ✅ implemented (Charlie closes alone via `validate_due=false`)
- Timeout refund witness — required for sender fund recovery

**Protocol gaps** (block peer-to-peer operation):
- CBOR wire protocol — M1 implements the codec, M4 adds the transport
- Metering + pricing + netting — the billing engine, independent of channel crypto
- Bidirectional channel pairs — doubling the state machine
- Rollover — multi-channel management per direction
- Negotiation (Announce, PriceSheet, Accept) — session setup
- Bootstrap tokens — connectivity bootstrap, already designed

**Operational gaps** (block deployment):
- State persistence — IndexedDB in browser, file/DB in Rust
- Error handling and retry — mint failures, peer failures, keyset changes
- Channel recovery after reboot — ChannelSync protocol message
- Capacity growth — policy engine, not crypto

The crypto gaps are the highest priority for M3 completion. The protocol and operational gaps are M4+ concerns that build on the proven crypto foundation.
