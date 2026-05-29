# learnings notepad

## T2: Test Vector Capture (2025-05-10)

### cdk-spilman Low-Level API for Test Vectors
- `compute_channel_from_token` → returns `params_json`, `capacity`, `funding_token_amount`, `proofs_json` — all needed for downstream calls
- `create_funding_swap` → returns `swap_request_json` (contains blinded messages in `outputs`), `funding_secrets_json` (contains blinding factors and secrets)
- `complete_funding_swap` → returns `funding_proofs_json` — the unblinded proofs
- `create_unsigned_balance_update` → returns `message_hex`, `tweak_scalar_hex` — crypto intermediates for signing
- `create_signed_balance_update` → returns `signature` — the final Schnorr signature
- `channel_parameters_get_channel_id` → takes `params_json`, `channel_secret_hex`, `keyset_info_json`

### Key Type Relationships
- `cashu::nuts::SecretKey` wraps `bitcoin::secp256k1::SecretKey` (Deref). Use `*sk` to get inner.
- `cashu::nuts::PublicKey` wraps `bitcoin::secp256k1::PublicKey` (Deref).
- `cdk::secp256k1` re-exports `bitcoin::secp256k1`. Same `bitcoin v0.32.9` unified across the dep tree.
- `SharedSecret::new(pk, sk)` works directly with cashu wrapper types via Deref coercion.
- `bitcoin` crate NOT directly accessible from tollgate-net tests — must use `cdk::secp256k1` instead.

### Deterministic Key Construction
- `SecretKey::from_slice(&hex_bytes)` creates a deterministic key from a seed.
- All blinded messages from `create_funding_swap` are deterministic given same channel params.
- Blind signatures from mint are deterministic given same blinded messages and keyset.
- Input proofs from testnut are NOT deterministic (random secrets from mint).

### Output Path
- Test vectors write to `docs/private/demos/spilman-real/test-vectors.json` (relative to workspace root).
- Path resolved via `CARGO_MANIFEST_DIR` → `../../docs/private/demos/spilman-real/test-vectors.json`.

## T3: cashu-ts ESM Probe (2025-05-10)

### Import Strategy Findings
- `@cashu/cashu-ts@4.2.1` loads fine from esm.sh — 132 exports, includes CashuMint, CashuWallet
- `@noble/curves@2.2.0/secp256k1` loads fine — has getSharedSecret, schnorr.sign, schnorr.verify, secp256k1.utils
- `@noble/hashes@2.2.0/sha256` — PROBLEM: cannot import from esm.sh (no entry point for @noble/hashes)
  - WORKAROUND: Use `@noble/hashes@2.2.0/sha256` via `https://esm.sh/@noble/hashes@2.2.0/sha256` — try this in crypto tasks
  - FALLBACK: Use `https://esm.sh/sha256` npm package (different library but same SHA-256 algorithm)
  - BEST OPTION for crypto tasks: cashu-ts transitively depends on @noble/hashes — see if cashu-ts re-exports it
- `secp256k1.utils.bytesToHex` and `secp256k1.utils.hexToBytes` exist in noble/curves — use these instead of custom implementations
- ECDH works: `secp256k1.getSharedSecret(privkey, pubkey)` returns 65-byte uncompressed point (first byte 0x04)
- Schnorr works: `schnorr.sign(message, privateKey)` and `schnorr.verify(signature, message, publicKey)`
- Schnorr getPublicKey: `schnorr.getPublicKey(privateKey)` returns 32-byte x-only pubkey

## T6+T7+T8: Crypto Primitives (2025-05-10)

### cdk-spilman Rust Source (fetched from GitHub)
- `derive_blinding_scalar` (params.rs): `SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry_counter}")` with retry loop [0..255] checking scalar validity [1, n-1]
- `create_deterministic_output_with_blinding` (params.rs): nonce = `SHA256(channel_secret || "{channel_id}|{context}|{amount}|nonce|{index}")`, blinding = `SHA256(channel_secret || "{channel_id}|{context}|{amount}|blinding|{index}")`
- `blind_message` (cashu::dhke): `B_ = hash_to_curve(secret) + r * G` — standard Cashu DHKE, mint pubkey NOT used in blinding
- `hash_to_curve` (Cashu NUT-00): `SHA256("Secp256k1_HashToCurve_Cashu_" || secret || counter)`, try-and-increment for valid x-coordinate
- `construct_proofs` (bindings.rs): `C = C_ - r * K_a` per signature, delegates to `cashu::dhke::construct_proofs`
- `sign_with_tweaked_key_util` (bindings.rs): BIP-340 parity check → negate if odd Y → add tweak → Schnorr sign

### noble/curves v2.2.0 EC Point API
- `secp256k1.ProjectivePoint` for point operations
- `Point.fromHex(compressedHex)` — decompresses from 02/03 prefix
- `Point.BASE.multiply(scalar)` — scalar multiplication (Uint8Array or BigInt)
- `point.add(other)`, `point.negate()` — no `.subtract()`, use `p1.add(p2.negate())`
- `point.toRawBytes(true)` — 33-byte compressed point
- `secp256k1.CURVE.n` — group order as BigInt
- `schnorr.sign(msg, privKeyBytes)` → 64-byte Uint8Array signature
- `schnorr.verify(sigBytes, msgBytes, pubXBytes)` → boolean
- `schnorr.getPublicKey(privKeyBytes)` → 32-byte x-only pubkey

### Key Simplifications in JS Demo
- `createSignedBalanceUpdate` uses simplified message hash `SHA256(channel_id + "|" + balance)` instead of full `sig_all_message_hash` over swap request (which requires constructing commitment outputs)
- The key tweaking and Schnorr signing logic is exact — only the message differs
- `blindMessage` accepts `mintPubkeyForAmount` parameter for API compatibility but uses standard Cashu formula `B_ = Y + r*G`

## T15: GitHub Pages CI Deploy Sub-Path Update (2025-05-10)

### CI Workflow Changes
- Added `actions/checkout@v5` step with `fetch-depth: 0` to `build-pages` job
- Added `Copy spilman-real browser demo` step before `Upload Pages artifact`
- Copy command: `cp -r docs/private/demos/spilman-real pages/spilman-real`

### Resulting Artifact Structure
- GitHub Pages URL: `https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/`
- Preserves directory structure:
  - index.html
  - style.css
  - src/ (channel.js, crypto.js, main.js, mint.js, ui.js, wallet.js)
  - README.md
  - test-vectors.json (if present)

### Important Notes
- Existing trace-based `index.html` generation is NOT disrupted
- `pages/index.html` is regenerated by the "Build unified protocol trace page" step
- The spilman-real demo is a separate artifact at `/spilman-real/`
- The simulator at `/spilman-simulator.html` continues to work (regression)

## T16: E2E Playwright QA — 8 Integration Bugs Found & Fixed (2025-05-10)

### @noble/curves v2.2.0 API Breaking Changes (3 bugs)
- `Point.multiply()` requires `bigint`, NOT `Uint8Array` — wrap with `bytesToBigInt()`
- `point.toRawBytes(true)` renamed to `point.toBytes(true)` in v2.2.0
- These are sequential in the execution path — each fix reveals the next

### testnut.cashu.exchange API Quirks (2 bugs)
- Mint quote state uses `{ "state": "PAID" }` not `{ "paid": true }` — check both fields
- Swap requires `sum(outputs) == sum(inputs) - fee` where `fee = ceil(total * ppk / 1000)`
- Minimum quote amount: 100 sat works fine (no need for +1000 buffer)

### Cashu hashToCurve Algorithm (1 critical bug)
- CDK uses TWO-STEP hash: `msg_hash = SHA256(prefix || message)`, then loop: `SHA256(msg_hash || counter_u32_LE)`
- NOT single-step: `SHA256(prefix || message || counter_byte)` (which was the original JS implementation)
- Counter is 4-byte LITTLE-ENDIAN u32, not 1 byte
- Only even y-parity (02 prefix), not both 02 and 03
- Test vector: `hashToCurve(32 zero bytes) = 024cce997d3b518f739663b757deaec95bcd9473c30a14ac2fd04023a739d1a725`

### fetch() Options Spread Pattern (1 bug)
- `fetch(url, { body: jsonString, ...options })` overwrites stringified body with raw object from options
- Fix: destructure `{ method: _m, headers: _h, body: _b, ...rest } = options` before spreading

### Secret Encoding in createDeterministicOutput (1 bug)
- `createDeterministicSecret` returns hex string — must `hexToBytes()` before passing to `hashToCurve`
- `TextEncoder.encode(hexString)` produces 64 ASCII bytes (wrong), `hexToBytes(hexString)` produces 32 raw bytes (correct)

### Fee Accounting in Cooperative Close
- Mint charges `input_fee_ppk` per 1000 sat on swap inputs
- `fee = ceil(inputTotal * inputFeePpk / 1000)` — must subtract from Alice's refund
- With `input_fee_ppk=10` and 100 sat inputs: fee = ceil(100*10/1000) = 1 sat
