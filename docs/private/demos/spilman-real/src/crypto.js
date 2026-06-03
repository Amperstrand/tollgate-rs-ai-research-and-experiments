// crypto.js — JS implementations of cdk-spilman crypto primitives
// Reference: https://github.com/SatsAndSports/cashu_spilman_channels/blob/main/crates/cdk-spilman/src/params.rs
//
// NUT-XX naming convention (from https://github.com/cashubtc/nuts/pull/296):
//   Alice  = sender   (pays into the channel)
//   Charlie = receiver (receives payments)
//   Bob    = mint     (holds the ecash, processes swaps)
//
// ARCHITECTURE (Wave C):
//   wallet.js delegates channel ops (secret, ID, funding, proofs, signing) to cdk-wasm.
//   This file provides what WASM doesn't cover:
//     - Key generation (generatePrivateKey, getPublicKey) — no WASM binding
//     - Denomination splitting (getDenominationAmounts) — not in WASM
//     - Close output construction (createDeterministicOutput) — "receiver"/"sender" contexts not in WASM
//     - Hex/SHA-256 utilities (re-exported from @noble)
//
//   test-vectors.js imports additional functions from this file to validate that
//   the JS implementations produce output identical to the Rust reference.
//   Those functions are NOT used by wallet.js but ARE needed for test vector validation.
//   They are marked with [TEST-VECTORS] below.
//
//   Internal helpers (not exported) are used by both wallet-used and test-vector functions.

import { secp256k1, schnorr } from "https://esm.sh/@noble/curves@2.2.0/secp256k1";
import { sha256 } from "https://esm.sh/@noble/hashes@1.7.1/sha256";
import { bytesToHex, hexToBytes } from "https://esm.sh/@noble/hashes@1.7.1/utils";

// Re-export hex utilities — used by wallet.js, cdk-wasm-adapter.js, test-vectors.js
export { bytesToHex, hexToBytes, sha256 };
// schnorr re-export kept for test-vectors.js only (wallet.js uses WASM for signing)
export { schnorr };

// ─── Key Generation ───────────────────────────────────────────────

/** Generate a random secp256k1 private key (Uint8Array, 32 bytes) */
export function generatePrivateKey() {
  return secp256k1.utils.randomSecretKey();
}

/** Get compressed public key from private key bytes (Uint8Array, 33 bytes) */
export function getPublicKey(privKeyBytes) {
  return secp256k1.getPublicKey(privKeyBytes, true);
}

// ─── ECDH Shared Secret ──────────────────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js. wallet.js uses WASM compute_channel_secret instead.

/** Compute raw ECDH shared secret (x-coordinate, 32 bytes) */
export function computeRawEcdh(mySecretKeyHex, theirPublicKeyHex) {
  const privBytes = hexToBytes(mySecretKeyHex);
  const pubBytes = hexToBytes(theirPublicKeyHex);
  const shared = secp256k1.getSharedSecret(privBytes, pubBytes);
  return shared.slice(1, 33);
}

// ─── Channel Secret ──────────────────────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js. wallet.js uses WASM compute_channel_secret instead.
// Rust source: params.rs:94-103 compute_channel_secret()
//   SHA256("Cashu_Spilman_channel_secret_v1" || ECDH_x_coordinate)

export function computeChannelSecret(mySecretKeyHex, theirPublicKeyHex) {
  const rawEcdh = computeRawEcdh(mySecretKeyHex, theirPublicKeyHex);
  const prefix = new TextEncoder().encode("Cashu_Spilman_channel_secret_v1");
  const input = new Uint8Array(prefix.length + rawEcdh.length);
  input.set(prefix, 0);
  input.set(rawEcdh, prefix.length);
  return bytesToHex(sha256(input));
}

// ─── Channel ID ──────────────────────────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js. wallet.js uses WASM channel_parameters_get_channel_id instead.
// Rust source: params.rs:485-507 get_channel_id_bytes() / get_channel_id()
//   SHA256("mint|unit|capacity|funding_token_amount|keyset_id|input_fee_ppk|max|setup_ts|sender_pk|receiver_pk|expiry_ts|channel_secret")

export function getChannelId(params, channelSecretHex) {
  const parts = [
    params.mint, params.unit, params.capacity, params.fundingTokenAmount,
    params.keysetId, params.inputFeePpk, params.maximumAmount, params.setupTimestamp,
    params.senderPubkey, params.receiverPubkey, params.expiryTimestamp,
    channelSecretHex,
  ];
  return bytesToHex(sha256(new TextEncoder().encode(parts.join("|"))));
}

// ─── EC Point Arithmetic (internal) ──────────────────────────────

const Point = secp256k1.Point;
const GROUP_ORDER = secp256k1.Point.Fn.ORDER;

function bytesToBigInt(bytes) {
  return BigInt("0x" + bytesToHex(bytes));
}

function bigIntToBytes32(n) {
  const hex = n.toString(16).padStart(64, "0");
  return hexToBytes(hex);
}

// ─── P2BK Blinding Scalar ────────────────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js for createSignedBalanceUpdate.
// Also called internally by createSignedBalanceUpdate below.
// wallet.js/cdk-wasm-adapter.js has its own deriveBlindingScalar for cooperative close.
// Rust source: params.rs:539-562 derive_blinding_scalar()
//   SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry}")

export function deriveBlindingScalar(channelSecret, channelId, context) {
  const prefix = new TextEncoder().encode("Cashu_Spilman_P2BK_v1");
  for (let retryCounter = 0; retryCounter <= 255; retryCounter++) {
    const text = new TextEncoder().encode(`${channelId}|${context}|${retryCounter}`);
    const input = new Uint8Array(prefix.length + channelSecret.length + text.length);
    input.set(prefix, 0);
    input.set(channelSecret, prefix.length);
    input.set(text, prefix.length + channelSecret.length);
    const hash = sha256(input);
    const hashInt = bytesToBigInt(hash);
    if (hashInt > 0n && hashInt < GROUP_ORDER) return hash;
  }
  throw new Error("Failed to derive valid blinding scalar after 256 attempts");
}

// ─── Deterministic Output Construction ───────────────────────────
// [TEST-VECTORS] createDeterministicSecret, createDeterministicBlindingFactor, blindMessage
// used by test-vectors.js. createDeterministicOutput used by wallet.js for close contexts.
// Rust source: params.rs:888-936 create_deterministic_output_with_blinding()
//   nonce:    SHA256(channel_secret || "{channel_id}|{context}|{amount}|nonce|{index}")
//   blinding: SHA256(channel_secret || "{channel_id}|{context}|{amount}|blinding|{index}")

export function createDeterministicSecret(channelSecret, channelId, context, amount, index) {
  const text = new TextEncoder().encode(`${channelId}|${context}|${amount}|nonce|${index}`);
  const input = new Uint8Array(channelSecret.length + text.length);
  input.set(channelSecret, 0);
  input.set(text, channelSecret.length);
  return bytesToHex(sha256(input));
}

export function createDeterministicBlindingFactor(channelSecret, channelId, context, amount, index) {
  const text = new TextEncoder().encode(`${channelId}|${context}|${amount}|blinding|${index}`);
  const input = new Uint8Array(channelSecret.length + text.length);
  input.set(channelSecret, 0);
  input.set(text, channelSecret.length);
  const hash = sha256(input);
  const hashInt = bytesToBigInt(hash);
  if (hashInt === 0n || hashInt >= GROUP_ORDER) {
    throw new Error("Invalid blinding factor derived (out of scalar range)");
  }
  return hash;
}

// hashToCurve — Cashu DHKE try-and-increment (internal, used by blindMessage)
function hashToCurve(message) {
  const prefix = new TextEncoder().encode("Secp256k1_HashToCurve_Cashu_");
  const prefixInput = new Uint8Array(prefix.length + message.length);
  prefixInput.set(prefix, 0);
  prefixInput.set(message, prefix.length);
  const msgHash = sha256(prefixInput);
  for (let counter = 0; counter < 65536; counter++) {
    const counterBytes = new Uint8Array(4);
    new DataView(counterBytes.buffer).setUint32(0, counter, true);
    const hashInput = new Uint8Array(32 + 4);
    hashInput.set(msgHash, 0);
    hashInput.set(counterBytes, 32);
    const hash = sha256(hashInput);
    try {
      return Point.fromHex("02" + bytesToHex(hash));
    } catch { /* x not on curve, increment counter */ }
  }
  throw new Error("hashToCurve: failed to find valid point after 65536 attempts");
}

// [TEST-VECTORS] blindMessage used by test-vectors.js via createDeterministicOutput
export function blindMessage(secretBytes, blindingScalar, _mintPubkeyForAmount) {
  const Y = hashToCurve(secretBytes);
  const rPoint = Point.BASE.multiply(bytesToBigInt(blindingScalar));
  const B_ = Y.add(rPoint);
  return bytesToHex(B_.toBytes(true));
}

// ─── Denomination Splitting ──────────────────────────────────────
// Used by wallet.js for cooperative close output construction.

export function getDenominationAmounts(targetAmount, maxPerOutput) {
  if (targetAmount === 0) return [];
  const amounts = [];
  let remaining = targetAmount;
  let denom = 1;
  while (denom * 2 <= maxPerOutput) denom *= 2;
  while (remaining > 0 && denom >= 1) {
    if (denom <= remaining) {
      amounts.push(denom);
      remaining -= denom;
    } else {
      denom = Math.floor(denom / 2);
    }
  }
  if (remaining > 0) {
    throw new Error(`Cannot represent remaining ${remaining} sat with denominations up to ${maxPerOutput}`);
  }
  return amounts;
}

// Used by wallet.js for cooperative close "receiver"/"sender" output construction
export function createDeterministicOutput(channelSecret, channelId, context, amount, index) {
  const secret = createDeterministicSecret(channelSecret, channelId, context, amount, index);
  const blindingFactor = createDeterministicBlindingFactor(channelSecret, channelId, context, amount, index);
  const secretBytes = hexToBytes(secret);
  const B_ = blindMessage(secretBytes, blindingFactor, "");
  return { secret, blindingFactor: bytesToHex(blindingFactor), B_, amount };
}

// ─── Unblinding + Proof Construction ─────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js. wallet.js uses WASM construct_proofs instead.

export function unblindSignature(blindSignatureHex, blindingScalar, mintPubkeyHex) {
  const C_ = Point.fromHex(blindSignatureHex);
  const K_a = Point.fromHex(mintPubkeyHex);
  const rK_a = K_a.multiply(bytesToBigInt(blindingScalar));
  const C = C_.add(rK_a.negate());
  return bytesToHex(C.toBytes(true));
}

export function constructProofs(blindSignatures, secretsWithBlinding, keysetId, mintKeys) {
  if (blindSignatures.length !== secretsWithBlinding.length) {
    throw new Error(`Signature count ${blindSignatures.length} != secret count ${secretsWithBlinding.length}`);
  }
  return blindSignatures.map((sig, i) => {
    const swb = secretsWithBlinding[i];
    const mintPubkeyHex = mintKeys[String(swb.amount)];
    if (!mintPubkeyHex) throw new Error(`No mint key for amount ${swb.amount}`);
    const blindingScalar = typeof swb.blinding_factor === "string"
      ? hexToBytes(swb.blinding_factor) : swb.blinding_factor;
    const C = unblindSignature(sig.C_, blindingScalar, mintPubkeyHex);
    return { amount: swb.amount, secret: swb.secret, C, id: keysetId };
  });
}

// ─── Schnorr-Signed Balance Update ───────────────────────────────
// [TEST-VECTORS] Used by test-vectors.js. wallet.js uses WASM spilman_channel_sender_create_signed_balance_update instead.

export function createSignedBalanceUpdate(_params, aliceSecretHex, channelSecretHex, channelIdHex, balanceToReceiver) {
  const channelSecret = hexToBytes(channelSecretHex);
  const tweakScalar = deriveBlindingScalar(channelSecret, channelIdHex, "sender_stage1");
  const tweakBigInt = bytesToBigInt(tweakScalar);
  const aliceSecret = hexToBytes(aliceSecretHex);
  const alicePubCompressed = secp256k1.getPublicKey(aliceSecret, true);
  const parityIsOdd = alicePubCompressed[0] === 0x03;
  let effectiveSecretBigInt = bytesToBigInt(aliceSecret);
  if (parityIsOdd) effectiveSecretBigInt = GROUP_ORDER - effectiveSecretBigInt;
  const tweakedBigInt = (effectiveSecretBigInt + tweakBigInt) % GROUP_ORDER;
  const tweakedBytes = bigIntToBytes32(tweakedBigInt);
  const messageHash = sha256(new TextEncoder().encode(`${channelIdHex}|${balanceToReceiver}`));
  const signature = schnorr.sign(messageHash, tweakedBytes);
  const tweakedPub = schnorr.getPublicKey(tweakedBytes);
  return {
    messageHex: bytesToHex(messageHash),
    signatureHex: bytesToHex(signature),
    tweakedPubHex: bytesToHex(tweakedPub),
  };
}
