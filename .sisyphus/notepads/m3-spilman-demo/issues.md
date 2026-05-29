# Issues

## T16: mintFetch body serialization bug (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/mint.js`, line 28  
**Severity:** CRITICAL — blocks all POST requests to the mint

### Symptom

POST to `https://testnut.cashu.exchange/v1/mint/quote/bolt11` returns:
```
500 {"error":"internal_error","detail":"Failed to create mint quote: Invalid JSON in request body","code":10000}
```

### Root Cause

In `mintFetch()` (mint.js:6-38), the function correctly stringifies the body on lines 14-20:
```js
body = JSON.stringify(options.body); // → '{"unit":"sat","amount":1100}' (string)
```

But then on line 28, the `...options` spread overwrites the stringified body:
```js
const response = await fetch(url, {
    method,
    headers,
    body,          // ← stringified JSON string (CORRECT)
    ...options,    // ← { method: "POST", body: { unit: "sat", amount: 1100 } }
                   //    body gets OVERWRITTEN back to raw object!
});
```

The `fetch()` API receives an object as body, converts to `[object Object]`, which the mint rejects.

### Verification

Reproduced in browser evaluate:
```
stringifiedBody: '{"unit":"sat","amount":1100}' (string)
fetchArgsBody:   { unit: "sat", amount: 1100 }  (object) ← BUG: overwritten by spread
bodiesMatch:     false
```

### Fix

Remove `...options` from the fetch call, or destructure options to exclude body/headers:
```js
const { method: _, headers: __, body: ___, ...fetchOptions } = options;
const response = await fetch(url, {
    method,
    headers,
    body,
    ...fetchOptions,
});
```

### Impact

All mint POST operations are broken: mint quote, mint bolt11, swap, checkstate. The lifecycle cannot progress past Phase 1 (Open Channel) because Phase 2 (Fund Channel) requires a mint quote.

### Evidence

- Screenshots: `task-16-initial.png`, `task-16-error-state.png`
- Lifecycle stuck at: Phase 1 complete (channel ID derived), Phase 2 failed
- Channel state: both wallets show `status: "INIT"`, capacity 100, balance 0

---

## T16-r2: pollMintQuote checks wrong field (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/mint.js`, line 109  
**Severity:** CRITICAL — funding always times out even after mint.js body fix

### Symptom

After fixing Bug #1 (body serialization), lifecycle still fails at Phase 2:
```
pollMintQuote timeout after 30000ms
```

### Root Cause

`pollMintQuote()` checks `state.paid` (boolean), but the testnut API returns `state.state` (string `"PAID"`):

```js
// Line 109 — WRONG
if (state.paid) {
    return state;
}
```

The actual API response shape:
```json
{
    "quote": "...",
    "request": "...",
    "state": "PAID",    ← string, not boolean
    "amount": 1100,
    "unit": "sat",
    "expiry": 1778418270
}
```

There is NO `paid` field. `state.paid` is always `undefined` → `!!undefined` = `false` → poll never detects payment.

### Verification

Browser evaluate confirms:
```
hasPaidField: false        ← no "paid" key in response
stateValue: "PAID"         ← the actual field is "state"
checkPassed: false         ← !!stateData.paid = false (wrong check)
checkShouldBe: true        ← stateData.state === 'PAID' = true (correct check)
```

Also confirmed: direct `curl` and `fetch` from browser both see quotes paid within 3 seconds. The mint auto-pay works fine — the code just never detects it.

### Fix

Change line 109 to check the correct field:
```js
// Option A: check both for compatibility
if (state.paid || state.state === 'PAID') {

// Option B: just check the state string
if (state.state === 'PAID') {
```

### Impact

Lifecycle can never proceed past Phase 2 (Fund Channel). The mint quote IS created, IS auto-paid by testnut, but the poll loop never detects the paid state. Every run times out at 30s.

### Evidence

- Screenshot: `task-16-r2-poll-bug.png`
- Debug panel shows: Phase 1 complete, Phase 2 starts, then "pollMintQuote timeout after 30000ms"
- Direct browser evaluate proves the API returns `state: "PAID"` not `paid: true`

---

## T16-r3: Point.BASE.multiply expects bigint, gets Uint8Array (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/crypto.js`, lines 271, 379  
**Severity:** CRITICAL — Phase 2 crashes after mint quote succeeds

### Symptom

After fixing Bugs #1 and #2, lifecycle now creates a paid mint quote successfully but crashes:
```
TypeError: invalid field element: expected bigint, got object
    at q.isValid (modular.mjs)
    at g.multiply (weierstrass.mjs)
    at blindMessage (crypto.js:271)
    at Module.createDeterministicOutput (crypto.js:349)
    at Object.fundChannel (wallet.js:105)
```

### Root Cause

`@noble/curves` v2.2.0 `Point.BASE.multiply()` expects a `bigint` scalar, but `createDeterministicBlindingFactor()` returns `Uint8Array` (raw SHA-256 hash bytes):

```js
// crypto.js:202-221 — createDeterministicBlindingFactor
const hash = sha256(input);        // → Uint8Array (32 bytes)
const hashInt = bytesToBigInt(hash); // validates range
return hash;                         // ← returns Uint8Array, not bigint!
```

Then in `blindMessage`:
```js
// crypto.js:271
const rPoint = Point.BASE.multiply(blindingScalar); // ← Uint8Array, needs bigint!
```

And in `unblindSignature`:
```js
// crypto.js:379
const rK_a = K_a.multiply(blindingScalar); // ← same issue
```

### Call Chain

1. `wallet.js:105` → `createDeterministicOutput(channelSecret, channelId, "funding", amounts[i], i)`
2. `crypto.js:341` → `createDeterministicBlindingFactor(...)` returns `Uint8Array`
3. `crypto.js:349` → `blindMessage(secretBytes, blindingFactor, "")` 
4. `crypto.js:271` → `Point.BASE.multiply(blindingScalar)` → **CRASH: expects bigint**

### Verification

Browser evaluate confirms:
```
Point.BASE.multiply(Uint8Array) → TypeError: "invalid field element: expected bigint, got object"
Point.BASE.multiply(1n)         → works
```

### Fix

Two options:

**Option A (minimal — convert at multiply call sites):**
```js
// crypto.js:271 (blindMessage)
const rPoint = Point.BASE.multiply(bytesToBigInt(blindingScalar));

// crypto.js:379 (unblindSignature)  
const rK_a = K_a.multiply(bytesToBigInt(blindingScalar));
```

**Option B (cleaner — return bigint from createDeterministicBlindingFactor):**
```js
// crypto.js:220 — change return type
return hashInt;  // was: return hash;
```
Then update callers:
- `crypto.js:349`: `blindMessage(secretBytes, blindingFactor, "")` — needs to handle bigint
- `crypto.js:352`: `blindingFactor: bytesToHex(blindingFactor)` — needs bigintToHex
- `crypto.js:416`: `hexToBytes(swb.blinding_factor)` — callers pass hex string anyway

**Recommended:** Option A is safest — minimal changes, doesn't cascade type changes.

### Impact

Phase 2 (Fund Channel) crashes immediately after mint quote is paid. The deterministic output creation fails, so no blinded messages are produced, no proofs are minted, no funding happens.

### Evidence

- Screenshot: `task-16-r3-bigint-bug.png`
- Debug panel: Phase 1 complete, Phase 2 starts, then "ERROR: invalid field element: expected bigint, got object"
- Browser evaluate confirms `multiply(bigint)` works, `multiply(Uint8Array)` fails

---

## T16-r4: Point.toRawBytes() doesn't exist in @noble/curves v2.2.0 (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/crypto.js`, lines 273, 382  
**Severity:** CRITICAL — Phase 2 crashes at blindMessage after bigint fix

### Symptom

After fixing Bugs #1-3, lifecycle fails immediately at Phase 2:
```
TypeError: B_.toRawBytes is not a function
    at blindMessage (crypto.js:273)
    at createDeterministicOutput (crypto.js:349)
    at fundChannel (wallet.js:105)
```

### Root Cause

`@noble/curves` v2.2.0 renamed `toRawBytes()` → `toBytes()`. The code uses the v1 API:

```js
// crypto.js:273 (blindMessage) — WRONG
return bytesToHex(B_.toRawBytes(true));

// crypto.js:382 (unblindSignature) — WRONG
return bytesToHex(C.toRawBytes(true));
```

In v2.2.0, the Point class has:
- `toBytes(isCompressed)` → Uint8Array (33 bytes compressed) ← USE THIS
- `toHex(isCompressed)` → hex string
- ~~`toRawBytes()`~~ → DOES NOT EXIST

### Verification

Browser evaluate confirms:
```js
const sum = Point.BASE.multiply(2n).add(Point.BASE.multiply(3n));
sum.toRawBytes(true)  // → TypeError: not a function
sum.toBytes(true)     // → Uint8Array(33) ✅
sum.toHex(true)       // → "02c604..." ✅
```

Point prototype methods: `toBytes`, `toHex`, `toString` (no `toRawBytes`).

### Fix

Replace `toRawBytes` with `toBytes` in two locations:
```js
// crypto.js:273
return bytesToHex(B_.toBytes(true));     // was: toRawBytes(true)

// crypto.js:382  
return bytesToHex(C.toBytes(true));      // was: toRawBytes(true)
```

Alternatively, could use `toHex()` directly:
```js
return B_.toHex(true);    // already returns hex string
```

### Impact

Phase 2 (Fund Channel) crashes at `blindMessage`. The blinded message B_ is computed correctly (the point arithmetic works), but serialization fails. No blinded outputs are produced, so no mint/bolt11 call happens.

### Evidence

- Screenshot: `task-16-r4-torawbytes-bug.png`
- Debug panel: Phase 1 complete, Phase 2 starts, "ERROR: B_.toRawBytes is not a function"
- Browser evaluate: `toBytes(true)` returns Uint8Array(33), `toHex(true)` returns 66-char hex

---

## T16-r5: Mint quote/output amount mismatch (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/wallet.js`, line 93  
**Severity:** CRITICAL — mint rejects the mint/bolt11 request

### Symptom

After fixing Bugs #1-4, Phase 2 progresses past quote creation and payment but fails at the actual mint call:
```
mint POST /v1/mint/bolt11 failed: 400 {"error":"invalid_amount","detail":"Amount mismatch: 100 !== 1100","code":11006}
```

### Root Cause

The code creates a mint quote for `capacitySat + 1000` (1100 sat) but only provides blinded outputs totaling `capacitySat` (100 sat):

```js
// wallet.js:93 — quote for 1100 sat
const quote = await mint.postMintQuoteBolt11(capacitySat + 1000);

// wallet.js:99 — outputs only total 100 sat
const amounts = crypto.getDenominationAmounts(capacitySat, this.channel.params.maximumAmount);
```

The Cashu NUT-03 spec requires: `sum(output.amounts) == quote.amount`. The mint validates this and rejects the request.

The README says: "Alice mints capacity + 1000 sat because testnut has a minimum, but only uses capacity sat worth of outputs." This is incorrect — testnut has no minimum (100 sat quotes work fine).

### Verification

```bash
# 100 sat quote — works
curl -s -X POST https://testnut.cashu.exchange/v1/mint/quote/bolt11 \
  -H "Content-Type: application/json" -d '{"unit":"sat","amount":100}'
# → {"quote":"...","state":"UNPAID","amount":100}

# Paid within 4 seconds
```

### Fix

Remove the `+ 1000` buffer — quote for exactly the capacity amount:
```js
// wallet.js:93
const quote = await mint.postMintQuoteBolt11(capacitySat);  // was: capacitySat + 1000
```

### Impact

Phase 2 (Fund Channel) fails at the `/v1/mint/bolt11` POST. The blinded outputs are correctly generated but the mint rejects them because their total (100) doesn't match the quote amount (1100). No proofs are minted.

### Evidence

- Screenshot: `task-16-r5-amount-mismatch.png`
- Debug panel: Phase 1 complete, Phase 2 starts, quote paid, then "ERROR: Amount mismatch: 100 !== 1100"
- Direct curl confirms 100 sat quotes work and get auto-paid

---

## T16-r6: hashToCurve receives UTF-8 hex string instead of raw bytes (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/crypto.js`, line 348  
**Severity:** CRITICAL — proofs fail mint verification, swap rejected

### Symptom

After fixing Bugs #1-5, Phases 1-4 all succeed, but Phase 5 (Cooperative Close) fails:
```
mint POST /v1/swap failed: 400 {"error":"invalid_request","detail":"Proof verification failed at index 0: Invalid proof signature at index 0","code":14005}
```

### Progress (first time past Phase 2!)
- ✅ Phase 1: Channel Open
- ✅ Phase 2: Fund Channel (3 proofs minted successfully)
- ✅ Phase 3: Payment 1 (10 sat)
- ✅ Phase 4: Payment 2 (20 sat) 
- ❌ Phase 5: Cooperative Close — swap 400

### Root Cause

In `createDeterministicOutput()` (crypto.js:348), the secret hex string is encoded as UTF-8 text before being passed to `hashToCurve`:

```js
// crypto.js:348 — WRONG
const secretBytes = new TextEncoder().encode(secret);
```

`secret` is a 64-char hex string like `"a1b2c3..."`. `TextEncoder.encode()` converts each ASCII character to a byte, producing 64 bytes. But the Cashu spec (NUT-00) expects `hashToCurve` to receive the *raw decoded bytes* of the secret (32 bytes from hex decoding).

**What happens:**
1. JS code: `hashToCurve(TextEncoder.encode("a1b2c3..."))` → hashes 64 ASCII bytes → point Y₁
2. Mint verification: `hashToCurve(hex_decode("a1b2c3..."))` → hashes 32 raw bytes → point Y₂
3. Y₁ ≠ Y₂ → proof C doesn't match → "Invalid proof signature"

The blinding math is correct, but the starting point (Y) is wrong because the input is wrong.

### Verification

```js
const secretHex = "a1b2c3d4e5f6...";
TextEncoder.encode(secretHex)  // → 64 bytes [97, 49, 98, 50, ...] (ASCII "a1b2")
hexToBytes(secretHex)           // → 32 bytes [161, 178, 195, 212, ...] (raw 0xa1b2)
// Completely different!
```

### Fix

```js
// crypto.js:348 — decode hex to raw bytes, don't encode as UTF-8 text
const secretBytes = hexToBytes(secret);  // was: new TextEncoder().encode(secret)
```

`hexToBytes` is already imported from `@noble/hashes/utils`.

### Impact

All proofs generated during funding are invalid from the mint's perspective. The mint accepted the blind signatures (because blinding/unblinding math is internally consistent) but the resulting C values don't match what the mint computes for the same secret. Swap, spend, or any operation that submits proofs to the mint will fail.

### Evidence

- Screenshot: `task-16-r6-proof-verification-failed.png`
- Network: keysets(200), keys(200), quote(200), poll(200), mint/bolt11(200), swap(400)
- Debug: Phases 1-4 complete, Phase 5 fails at swap
- Browser evaluate confirms TextEncoder(64 bytes) ≠ hexToBytes(32 bytes)

---

## T16-r7: hashToCurve algorithm doesn't match Cashu NUT-00 / CDK spec (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/crypto.js`, lines 231-254  
**Severity:** CRITICAL — all proofs have wrong Y point, making them unspendable

### Symptom

After fixing Bugs #1-6 (including the hexToBytes fix for secret encoding), Phases 1-4 still pass but Phase 5 (swap) fails with the same error:
```
Proof verification failed at index 0: Invalid proof signature at index 0
```

A standalone end-to-end test (mint → unblind → swap) also fails, proving the issue is in the proof construction chain, not the demo orchestration.

### Root Cause

The `hashToCurve` implementation in crypto.js does NOT match the Cashu NUT-00 spec as implemented by the CDK (cashubtc/cdk). Two critical differences:

**JS implementation (WRONG):**
```js
// crypto.js:231-254
SHA256("Secp256k1_HashToCurve_Cashu_" || message || counter_u8)  // direct hash, 1-byte counter
// Tries both 02 and 03 prefixes
```

**CDK implementation (CORRECT) — from dhke.rs:**
```rust
// Step 1: intermediate hash
let msg_hash = SHA256(DOMAIN_SEPARATOR || message);  // 32 bytes
// Step 2: counter hash
let hash = SHA256(msg_hash || counter_u32_LE);        // two-step, 4-byte LE counter
// Always uses even y (02 prefix only, via XOnlyPublicKey + Parity::Even)
```

Three differences:
1. **No intermediate hash** — JS hashes everything in one pass; CDK does two-step
2. **Counter encoding** — JS uses 1-byte `counter`; CDK uses 4-byte little-endian `u32`
3. **Y parity** — JS tries both even/odd y; CDK always uses even y

### Verification with CDK test vector

```
Input: secret = hex_decode("0000000000000000000000000000000000000000000000000000000000000000")

Expected (CDK):  024cce997d3b518f739663b757deaec95bcd9473c30a14ac2fd04023a739d1a725
JS hashToCurve:  02f7fa6c59b888605fd01f44a87ef4cbe87bce9663bb1a1043a72d0610876a28fa
CDK algorithm:   024cce997d3b518f739663b757deaec95bcd9473c30a14ac2fd04023a739d1a725 ✅

JS matches expected:   false
CDK matches expected:  true
```

### Fix

Rewrite `hashToCurve` to match the CDK spec:
```js
function hashToCurve(messageBytes) {
  const prefix = new TextEncoder().encode("Secp256k1_HashToCurve_Cashu_");
  
  // Step 1: intermediate hash
  const step1 = new Uint8Array(prefix.length + messageBytes.length);
  step1.set(prefix, 0);
  step1.set(messageBytes, prefix.length);
  const msgHash = sha256(step1);
  
  // Step 2: counter-based try-and-increment
  for (let counter = 0; counter < 65536; counter++) {
    const bytesToHash = new Uint8Array(36); // 32 + 4
    bytesToHash.set(msgHash, 0);
    const dv = new DataView(bytesToHash.buffer);
    dv.setUint32(32, counter, true); // little-endian u32
    const hash = sha256(bytesToHash);
    const xHex = bytesToHex(hash);
    
    // Always use even y (02 prefix) — matches CDK's XOnlyPublicKey + Parity::Even
    try {
      return Point.fromHex("02" + xHex);
    } catch {
      // x not on curve, increment counter
    }
  }
  throw new Error("hashToCurve: failed to find valid point after 65536 attempts");
}
```

### Impact

Every proof generated by the demo has a completely wrong Y point. The blinding/unblinding math is internally consistent (so minting succeeds), but the resulting C values are computed from different Y points than what the mint computes. The mint's verification (`C == k * Y_cdk(secret)`) fails because `Y_js ≠ Y_cdk`.

This is the root cause of all proof verification failures. Without fixing hashToCurve, NO proofs will ever be spendable.

### Evidence

- Screenshot: `task-16-r7-hashtocurve-bug.png`
- CDK source: `cashubtc/cdk/crates/cashu/src/dhke.rs`
- CDK test vector: secret="00...00" → expected Y=`024cce997d...`
- Browser evaluate: JS produces `02f7fa6c...`, CDK algorithm produces `024cce99...` (matches expected)
- Standalone mint+unblind+swap test also fails (isolated from demo code)

---

## T16-r8: Cooperative close doesn't account for mint swap fees (BLOCKS lifecycle test)

**Date:** 2026-05-10  
**File:** `docs/private/demos/spilman-real/src/wallet.js`, line 225  
**Severity:** MODERATE — proofs are now valid, but swap amount doesn't leave room for fees

### Symptom

After fixing Bugs #1-7 (including hashToCurve), Phases 1-4 pass, Phase 5 reaches the swap endpoint, proof verification PASSES, but the swap fails:
```
mint POST /v1/swap failed: 400 {"error":"invalid_amount","detail":"inputs (100) - outputs (100) available for fees (0) is less than required (1).","code":11001}
```

### Root Cause

The cooperative close creates outputs that sum to exactly 100 sat (30 for Charlie + 70 for Alice), equal to the inputs. But the mint charges a fee based on `input_fee_ppk=10`:
- Fee = `ceil(input_total * input_fee_ppk / 1000)` = `ceil(100 * 10 / 1000)` = `ceil(1)` = 1 sat
- Required: `inputs - outputs >= fee` → `100 - 100 = 0 < 1` ❌

The code doesn't subtract the fee from Alice's refund:
```js
// wallet.js:225 — doesn't account for fees
const balanceToAlice = this.channel.capacity - balanceToCharlie;  // 100 - 30 = 70
```

Should be:
```js
const fee = Math.ceil(this.channel.params.inputFeePpk * inputTotal / 1000);
const balanceToAlice = this.channel.capacity - balanceToCharlie - fee;  // 100 - 30 - 1 = 69
```

### Fix

In `cooperativeClose()`, calculate the mint fee and subtract it from Alice's refund:
```js
// wallet.js:225 — account for mint swap fees
const inputTotal = this.channel.fundingProofs.reduce((s, p) => s + p.amount, 0);
const fee = Math.ceil(inputTotal * (this.channel.params.inputFeePpk || 0) / 1000);
const balanceToAlice = this.channel.capacity - balanceToCharlie - fee;
```

Expected result: Alice gets 69 sat, Charlie gets 30 sat, 1 sat goes to the mint as fee.

### Impact

The swap is rejected by the mint. All crypto is correct — proofs pass verification — but the economics don't account for the mint's fee. This is the last remaining issue before the full lifecycle completes.

### Evidence

- Screenshot: `task-16-r8-fee-issue.png`
- Debug: Phases 1-4 complete, Phase 5 swap returns 400 with fee error
- Error: "inputs (100) - outputs (100) available for fees (0) is less than required (1)"
- Keyset: `input_fee_ppk=10` → fee = ceil(100*10/1000) = 1 sat
