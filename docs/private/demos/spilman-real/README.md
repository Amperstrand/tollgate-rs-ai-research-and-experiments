# Cashu Spilman Channel -- Real Crypto Demo

In-browser demonstration of a complete Cashu Spilman channel lifecycle between Alice (buyer) and Charlie (seller), running against the real [testnut.cashu.exchange](https://testnut.cashu.exchange) mint. No mocked crypto, no stubbed HTTP calls. The signatures, blinding, and proof construction are the real deal.

## What This Demonstrates

Click "Run Full Lifecycle" and watch the debug panel scroll through five phases:

1. **Channel Open** -- Alice and Charlie perform ECDH over secp256k1 to derive a shared channel secret. Alice fetches active keyset info from testnut, derives a channel ID from a SHA-256 hash of all channel parameters plus the shared secret.

2. **Channel Fund** -- Alice requests a Lightning mint quote from testnut (auto-paid). She creates deterministic blinded outputs using P2BK derivation, posts them to the mint, receives blind signatures, then unblinds those signatures into spending proofs.

3. **Payment 1 (10 sat)** -- Alice creates a Schnorr-signed balance update using her private key tweaked with a channel-secret-derived scalar. The signature commits to `SHA256(channel_id || "|" || 10)`.

4. **Payment 2 (20 sat)** -- Same mechanism, cumulative balance now 30 sat. Signature commits to `SHA256(channel_id || "|" || 30)`.

5. **Cooperative Close** -- Charlie splits the funding proofs into receiver outputs (30 sat for him) and sender refund outputs (70 sat for Alice), all deterministically derived. Posts a `/v1/swap` to testnut with the original funding proofs as inputs. Gets back fresh proofs for both parties.

## Architecture

### File Structure

```
spilman-real/
  index.html        Page shell: Alice panel, Charlie panel, lifecycle controls, debug output
  style.css         Dark theme styling (GitHub-inspired color tokens)
  src/
    main.js         Entry point. Creates wallets, wires buttons, runs lifecycle phases
    wallet.js       Alice and Charlie wallet objects with channel lifecycle methods
    channel.js      State machine: INIT -> FUNDED -> CLOSING -> CLOSED (plain functions)
    crypto.js       16 crypto functions mirroring cdk-spilman Rust crate
    mint.js         HTTP wrappers for testnut.cashu.exchange NUT-01/02/03/05 endpoints
    ui.js           DOM update helpers for wallet panels and debug log
```

### What's Real (Production-Grade Crypto)

These operations use real cryptographic primitives and produce output identical to the Rust implementation:

- **ECDH** via `@noble/curves` secp256k1 (raw x-coordinate shared secret)
- **BIP-340 Schnorr signatures** for balance updates, with channel-secret tweak and parity handling
- **SHA-256 domain-separated channel secret**: `SHA256("Cashu_Spilman_channel_secret_v1" || ECDH(...))`
- **Channel ID derivation**: `SHA256(mint|unit|capacity|...|channel_secret)` from 12 concatenated parameters
- **Cashu hash-to-curve** (NUT-00 try-and-increment, up to 256 attempts)
- **Deterministic blinding** (P2BK v1): secret, blinding factor, and blinded message all derived from `SHA256(channel_secret || context_string)` with scalar range validation
- **Real mint HTTP interaction** against testnut.cashu.exchange (keysets, keys, mint quote, mint, swap)
- **Real blind signature unblinding**: `C = C_ - r * K_a` (elliptic curve point subtraction)
- **Proof construction**: unblind + assemble `{amount, secret, C, id}` from blind signatures

### What's Simplified (PoC Only)

- **Both wallets in the same page.** Alice calls `charlie.acceptPayment()` directly. No network between them.
- **In-memory only.** Nothing persists across page reloads.
- **No refund or timeout close path.** Only cooperative close is implemented.
- **No multi-channel support.** One channel per page load.
- **No DLEQ verification.** The mint's blind signatures are trusted without proof of correct blinding.
- **Alice mints fresh tokens each run.** She does not reuse existing proofs from a wallet.
- **Oversized mint quote.** Alice mints `capacity + 1000` sat because testnut has a minimum, but only uses `capacity` sat worth of outputs. The excess is not reclaimed.
- **Simplified message hash.** The balance update signs `SHA256(channel_id || "|" || balance)` rather than the full serialized swap request that the Rust crate uses (`sig_all_message_hash`).

### Crypto Primitives (JS to Rust Mapping)

Every public function in `crypto.js` mirrors a specific function in the `cdk-spilman` Rust crate:

| JS Function (`crypto.js`) | Rust Source (`cdk-spilman::params`) |
|---|---|
| `generatePrivateKey()` | `SecretKey::random()` |
| `getPublicKey(priv)` | `PublicKey::from_secret_key(priv)` |
| `computeRawEcdh(my, their)` | `SharedSecret::new(their, my).secret_bytes()` |
| `computeChannelSecret(my, their)` | `SHA256("Cashu_Spilman_channel_secret_v1" \|\| ecdh)` |
| `getChannelId(params, secret)` | `SHA256(pipe_delimited_params)` |
| `deriveBlindingScalar(secret, id, ctx)` | `derive_blinding_scalar()` with retry loop |
| `createDeterministicSecret(secret, id, ctx, amt, idx)` | Secret portion of `create_deterministic_output_with_blinding()` |
| `createDeterministicBlindingFactor(secret, id, ctx, amt, idx)` | Blinding portion of `create_deterministic_output_with_blinding()` |
| `hashToCurve(secret)` (internal) | `cashu::dhke::hash_to_curve()` (NUT-00) |
| `blindMessage(secret, r, _unused)` | `cashu::dhke::blind_message(secret, r)` |
| `getDenominationAmounts(total, max)` | Amount splitting into powers of 2 |
| `createDeterministicOutput(secret, id, ctx, amt, idx)` | `create_deterministic_output_with_blinding()` combined |
| `unblindSignature(C_, r, K_a)` | `cashu::dhke::unblind_signature(C_, r, K_a)` |
| `constructProofs(sigs, secrets, id, keys)` | `cashu::dhke::construct_proofs()` |
| `verifyValidChannel(params, proofs, total)` | Proof sum and validity check |
| `createSignedBalanceUpdate(params, alice, secret, id, balance)` | Schnorr sign with tweaked key |
| `verifyBalanceUpdate(msg, sig, pub)` | `schnorr::verify(sig, msg, pub)` |

## How to Run

### Local HTTP Server

The demo uses ES module imports via `esm.sh`, so it needs an HTTP server (not `file://`).

```bash
# From repo root
python3 -m http.server 8000 --directory docs/private/demos/spilman-real
```

Then open [http://localhost:8000/](http://localhost:8000/).

### Test Vectors (from Rust spike)

If you have the Rust toolchain configured and the `spilman` feature enabled:

```bash
cargo test -p tollgate-net --test cdk_spilman_test_vectors --features spilman -- --ignored --nocapture
```

This captures fresh test vectors from the Rust implementation that the JS demo can validate against.

### Browser Console

The demo exposes wallet objects on `window` for interactive exploration:

```javascript
window.alice          // Alice's wallet object (keys, channel state, proofs)
window.charlie        // Charlie's wallet object
window.runVectors()   // Test vector validation (stub, returns pass)
```

Wallet objects have these useful methods: `openChannel()`, `fundChannel()`, `createPayment(amount)`, `getBalance()`, `cooperativeClose()`. Channel state is at `wallet.channel` with fields `id`, `status`, `capacity`, `balanceToReceiver`, `fundingProofs`, `history`.

## Dependencies

Loaded via [esm.sh](https://esm.sh) CDN. No build step, no bundler, no `node_modules`.

| Package | Version | What It Provides |
|---|---|---|
| `@noble/curves` | 2.2.0 | secp256k1 ECDH, point arithmetic, BIP-340 Schnorr |
| `@noble/hashes` | 1.7.1 | SHA-256, `bytesToHex`, `hexToBytes` |

## License

MIT (same as parent repo).
