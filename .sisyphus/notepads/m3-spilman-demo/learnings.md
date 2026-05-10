# Learnings

## @noble/curves v2.2.0 API Changes from v1

- `Point.BASE.multiply()` requires `bigint`, NOT `Uint8Array`
- `toRawBytes()` removed → use `toBytes()` instead
- `Point` accessed via `secp256k1.Point` (not `ProjectivePoint`)
- `GROUP_ORDER` via `secp256k1.Point.Fn.ORDER`
- `randomSecretKey()` via `secp256k1.utils.randomSecretKey()`

## Cashu hashToCurve (NUT-00) — CDK Implementation

The Cashu NUT-00 hashToCurve is a **two-step** algorithm, NOT a direct hash:

1. `msg_hash = SHA256("Secp256k1_HashToCurve_Cashu_" || message)` — intermediate hash
2. For counter 0..65535: `hash = SHA256(msg_hash || counter_u32_LE)` — 4-byte LE counter
3. Try `02 || hash` as compressed point (even y only, matching CDK's `XOnlyPublicKey + Parity::Even`)

**NOT** the simple `SHA256(prefix || message || counter_byte)` that some docs suggest.

CDK test vector: `hashToCurve(32 zero bytes)` = `024cce997d3b518f739663b757deaec95bcd9473c30a14ac2fd04023a739d1a725`

## testnut.cashu.exchange Mint Behavior

- Auto-pays Lightning mint quotes within 3-4 seconds
- No minimum quote amount (100 sat works fine)
- Quote state field is `state: "PAID"` (string), NOT `paid: true` (boolean)
- `input_fee_ppk=10` → fee = ceil(amount * 10 / 1000) = 1 sat per 100 sat
- Active keyset: `008e808b89acc141`

## Cashu Secret Encoding

Secrets in proofs are hex strings (64 chars). When passing to `hashToCurve`, use `hexToBytes(secret)` to get 32 raw bytes, NOT `new TextEncoder().encode(secret)` which gives 64 UTF-8 bytes.

## ESM Module Caching

ESM modules loaded via `esm.sh` are cached aggressively. After code changes:
1. Clear browser cache via CDP: `Network.clearBrowserCache` + `Network.setCacheDisabled`
2. Hard reload the page
3. Wait 5 seconds for ESM modules to fully load
