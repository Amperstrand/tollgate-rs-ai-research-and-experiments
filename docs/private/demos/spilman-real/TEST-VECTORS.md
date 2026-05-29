# Test Vector Contract

This document defines the contract between the Rust test vector capture
(`cdk_spilman_test_vectors.rs`) and the JS demo validation. It specifies
what fields are captured, what assertions the JS side must make, and how
to regenerate vectors when the protocol changes.

## Generating Vectors

```bash
cargo test -p tollgate-net --test cdk_spilman_test_vectors \
  --features spilman -- --ignored --nocapture
```

Output: `docs/private/demos/spilman-real/test-vectors.json`

The test uses deterministic keys (`ALICE_SEED_HEX`, `CHARLIE_SEED_HEX`) but
mints real tokens from `testnut.cashu.exchange`. This means:
- **Stable across runs**: Key derivation, raw ECDH, and the domain-separated
  channel secret are fully deterministic.
- **Stable within one captured vector file**: Channel ID, funding outputs,
  blinded messages, and balance-update signatures must be reproduced exactly
  from the parameters captured in that same file.
- **Non-deterministic across fresh captures**: Mint keysets, selected proofs,
  expiry timestamps, funding proofs, blind signatures, and every value derived
  from those changing inputs may differ between captures.

### Deterministic Fields (byte-for-byte stable across runs)

| Field | Description |
|-------|-------------|
| `alice_seed_hex` | Alice's secret key (fixed: `1111...1111`) |
| `alice_pubkey_hex` | Derived from seed |
| `charlie_seed_hex` | Charlie's secret key (fixed: `2222...2222`) |
| `charlie_pubkey_hex` | Derived from seed |
| `ecdh_shared_secret_hex` | ECDH(alice_secret, charlie_pubkey) - x-coordinate |
| `channel_secret_hex` | SHA256("Cashu_Spilman_channel_secret_v1" \|\| ecdh) |
### Captured-Input Fields (byte-for-byte stable within one vector file)

| Field | Description |
|-------|-------------|
| `channel_id_hex` | Derived from captured `params_json`, keyset info, expiry, and channel secret |
| `funding_blinded_messages_sender_stage1` | Funding outputs derived from captured channel inputs |
| `signed_balance_update.message_hex` | Rust `sig_all_message_hash` for the captured funding proofs and balance |
| `signed_balance_update.signature_hex` | Schnorr signature over the captured message hash |
| `signed_balance_update.tweak_scalar_hex` | Derived blinding scalar for the captured balance update |
| `signed_balance_update.tweaked_pub_hex` | Alice's pubkey + captured tweak |

### Non-Deterministic Fields (change per run)

| Field | Why |
|-------|-----|
| `keyset_id` | Mint rotates keysets |
| `keyset_keys` | Mint keyset public keys change per keyset |
| `keyset_input_fee_ppk` | May change per keyset |
| `funding_blinded_messages_*` | Depend on minted proofs (which depend on quote) |
| `funding_blind_signatures_*` | Mint returns different signatures each time |
| `constructed_proofs_*` | Derived from blind signatures |
| `expiry_timestamp` | `now() + 3600` |
| `channel_id_hex` | Changes whenever captured parameters change |
| `signed_balance_update.*` | Changes whenever captured funding proofs or parameters change |

### Semi-Deterministic Fields

| Field | Condition |
|-------|-----------|
| `channel_id_hex` | Deterministic for one captured `params_json`. Changes if keyset, expiry, funding proofs, or params change. |
| `funding_amount_sat` | Deterministic given same capacity and keyset fee |
| `capacity_sat` | Deterministic (always equals requested capacity) |

## JSON Schema

```jsonc
{
  // Deterministic keys
  "alice_seed_hex": string,          // 64 hex chars
  "alice_pubkey_hex": string,        // 66 hex chars (compressed)
  "charlie_seed_hex": string,        // 64 hex chars
  "charlie_pubkey_hex": string,      // 66 hex chars (compressed)

  // ECDH derivation
  "ecdh_shared_secret_hex": string,  // 64 hex chars
  "channel_secret_hex": string,      // 64 hex chars

  // Channel parameters
  "params_json": string,             // serialized cdk-spilman channel params
  "keyset_info_json": string,        // serialized keyset info used by Rust
  "funding_proofs_json": string,     // serialized funding proofs used by Rust
  "channel_id_hex": string,          // 64 hex chars
  "keyset_id": string,               // varies
  "keyset_input_fee_ppk": number,
  "funding_amount_sat": number,
  "capacity_sat": number,
  "maximum_amount_per_output": number,
  "expiry_timestamp": number,        // unix seconds

  // Mint keyset keys (amount -> pubkey_hex)
  "keyset_keys": { [amount: string]: string },

  // Funding phase: blinded messages with full derivation info
  "funding_blinded_messages_sender_stage1": [
    {
      "amount": number,
      "B_": string,                    // 66 hex chars (blinded point)
      "blinding_factor_r": string,     // 64 hex chars (scalar)
      "secret": string                 // 64 hex chars (deterministic secret)
    }
  ],

  // Funding phase: mint's blind signatures
  "funding_blind_signatures_sender_stage1": [
    {
      "amount": number,
      "C_": string                     // 66 hex chars (blind signature point)
    }
  ],

  // Funding phase: constructed proofs after unblinding
  "constructed_proofs_sender_stage1": [
    {
      "amount": number,
      "secret": string,
      "C": string                      // 66 hex chars (proof commitment)
    }
  ],

  // Balance update (amount=30)
  "signed_balance_update": {
    "amount_to_charlie": 30,
    "message_hex": string,             // 64 hex chars
    "signature_hex": string,           // 128 hex chars (Schnorr)
    "tweak_scalar_hex": string,        // 64 hex chars
    "tweaked_pub_hex": string          // 64 hex chars (x-only)
  }
}
```

## JS Validation Assertions

The JS demo should load `test-vectors.json` and assert:

### 1. Key Derivation (fully deterministic)

```javascript
assert(bytesToHex(crypto.getPublicKey(hexToBytes(vectors.alice_seed_hex))) === vectors.alice_pubkey_hex);
assert(bytesToHex(crypto.getPublicKey(hexToBytes(vectors.charlie_seed_hex))) === vectors.charlie_pubkey_hex);
```

### 2. ECDH + Channel Secret (fully deterministic)

```javascript
const rawEcdh = crypto.computeRawEcdh(vectors.alice_seed_hex, vectors.charlie_pubkey_hex);
assert(bytesToHex(rawEcdh) === vectors.ecdh_shared_secret_hex);

const channelSecret = crypto.computeChannelSecret(vectors.alice_seed_hex, vectors.charlie_pubkey_hex);
assert(channelSecret === vectors.channel_secret_hex);
```

### 3. Channel ID (captured-input deterministic)

```javascript
const params = JSON.parse(vectors.params_json);
const channelId = crypto.getChannelId(params, channelSecret);
assert(channelId === vectors.channel_id_hex);
```

### 4. Deterministic Output Derivation (fully deterministic given inputs)

```javascript
for (const bm of vectors.funding_blinded_messages_sender_stage1) {
  const secret = crypto.createDeterministicSecret(
    channelSecretBytes, channelId, "sender_stage1", bm.amount, index
  );
  assert(secret === bm.secret);

  const bf = crypto.createDeterministicBlindingFactor(
    channelSecretBytes, channelId, "sender_stage1", bm.amount, index
  );
  assert(bytesToHex(bf) === bm.blinding_factor_r);

  const output = crypto.createDeterministicOutput(
    channelSecretBytes, channelId, "sender_stage1", bm.amount, index
  );
  assert(output.B_ === bm.B_);
}
```

### 5. Balance Update (captured-input deterministic)

```javascript
const update = crypto.createSignedBalanceUpdate(
  params, vectors.alice_seed_hex, channelSecret, channelId, 30
);
assert(update.messageHex === vectors.signed_balance_update.message_hex);
assert(update.signatureHex === vectors.signed_balance_update.signature_hex);
assert(update.tweakedPubHex === vectors.signed_balance_update.tweaked_pub_hex);
```

### 6. Proof Construction (non-deterministic - verify structure only)

```javascript
// Cannot assert exact values (blind sigs vary per run)
// Instead verify: correct number of proofs, amounts match, C is valid point
const proofs = crypto.constructProofs(sigs, secrets, keysetId, keys);
assert(proofs.length === expected_count);
for (const p of proofs) {
  assert(p.amount > 0);
  assert(p.secret.length === 64);
  assert(p.C.length === 66);
  // Verify C is a valid secp256k1 point
  secp256k1.Point.fromHex(p.C);
}
```

## Regeneration Protocol

When to regenerate test vectors:

1. **cdk-spilman version bump** - any change to the crypto crate
2. **Domain separation string change** - e.g., `v1` -> `v2` in any prefix
3. **Keyset rotation** - if deterministic assertions fail due to new keyset
4. **After any change to `cdk_spilman_test_vectors.rs`**

After regeneration:
1. Commit the new `test-vectors.json`
2. Run JS validation in browser console (`window.runVectors()`)
3. Verify all deterministic assertions still pass
4. If semi-deterministic fields changed (new keyset), update the JS test to
   skip those assertions or use the new keyset

## Drift Detection in CI

The CI oracle (Phase 1, ADR-0005) will:
1. Run `cdk_spilman_test_vectors` to generate fresh vectors
2. Load vectors in a headless browser (Playwright)
3. Call each `crypto.js` function with vector inputs
4. Assert byte-for-byte match on deterministic fields
5. Fail the build on any mismatch

This ensures that changes to `crypto.js` cannot silently diverge from the
Rust reference implementation.
