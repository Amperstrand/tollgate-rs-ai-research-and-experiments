// crypto.js — JS implementations of cdk-spilman crypto primitives
// Mirrors: https://github.com/SatsAndSports/cashu_spilman_channels/blob/main/crates/cdk-spilman/src/params.rs
import { secp256k1, schnorr } from "https://esm.sh/@noble/curves@2.2.0/secp256k1";
import { sha256 } from "https://esm.sh/@noble/hashes@2.2.0/sha256";
import { bytesToHex, hexToBytes } from "https://esm.sh/@noble/hashes@2.2.0/utils";

// Re-export hex utilities
export { bytesToHex, hexToBytes };
export { schnorr };

// ─── Key Generation ───────────────────────────────────────────────

/** Generate a random secp256k1 private key (Uint8Array, 32 bytes) */
export function generatePrivateKey() {
  return secp256k1.utils.randomPrivateKey();
}

/** Get compressed public key from private key bytes (Uint8Array, 33 bytes) */
export function getPublicKey(privKeyBytes) {
  return secp256k1.getPublicKey(privKeyBytes, true);
}

// ─── ECDH Shared Secret ──────────────────────────────────────────

/**
 * Compute raw ECDH shared secret (before domain separation).
 * Rust: SharedSecret::new(their_pubkey, my_secret).secret_bytes()
 * Returns 32 bytes (x-coordinate of the shared point).
 *
 * @param {string} mySecretKeyHex - 32-byte hex private key
 * @param {string} theirPublicKeyHex - 33-byte hex compressed public key
 * @returns {Uint8Array} 32-byte raw ECDH shared secret
 */
export function computeRawEcdh(mySecretKeyHex, theirPublicKeyHex) {
  const privBytes = hexToBytes(mySecretKeyHex);
  const pubBytes = hexToBytes(theirPublicKeyHex);
  // noble/curves getSharedSecret returns 65 bytes (uncompressed: 04 || x || y)
  // We need just the x-coordinate (32 bytes) to match secp256k1 ECDH behavior
  const shared = secp256k1.getSharedSecret(privBytes, pubBytes);
  // shared[0] = 0x04 (uncompressed marker), shared[1..33] = x, shared[33..65] = y
  return shared.slice(1, 33);
}

// ─── Channel Secret ──────────────────────────────────────────────

/**
 * Domain-separated channel secret.
 * Rust source (params.rs:94-103):
 *   SHA256("Cashu_Spilman_channel_secret_v1" || ECDH(my_secret, their_pubkey))
 *
 * @param {string} mySecretKeyHex - 32-byte hex private key
 * @param {string} theirPublicKeyHex - 33-byte hex compressed public key
 * @returns {string} 64-char hex string (32 bytes)
 */
export function computeChannelSecret(mySecretKeyHex, theirPublicKeyHex) {
  const rawEcdh = computeRawEcdh(mySecretKeyHex, theirPublicKeyHex);

  // SHA256("Cashu_Spilman_channel_secret_v1" || raw_ecdh)
  const prefix = new TextEncoder().encode("Cashu_Spilman_channel_secret_v1");
  const input = new Uint8Array(prefix.length + rawEcdh.length);
  input.set(prefix, 0);
  input.set(rawEcdh, prefix.length);

  const hash = sha256(input);
  return bytesToHex(hash);
}

// ─── Channel ID ──────────────────────────────────────────────────

/**
 * Derive channel ID from parameters.
 * Rust source (params.rs:485-502):
 *   SHA256("{mint}|{unit}|{capacity}|{funding_token_amount}|{keyset_id}|{input_fee_ppk}|{maximum_amount}|{setup_timestamp}|{sender_pubkey}|{receiver_pubkey}|{expiry_timestamp}|{channel_secret_hex}")
 *
 * @param {Object} params - Channel parameters
 * @param {string} params.mint - Mint URL
 * @param {string} params.unit - Currency unit ("sat")
 * @param {number} params.capacity - Channel capacity in sat
 * @param {number} params.fundingTokenAmount - Funding token amount
 * @param {string} params.keysetId - Keyset ID
 * @param {number} params.inputFeePpk - Input fee per kilo
 * @param {number} params.maximumAmount - Max amount per output
 * @param {number} params.setupTimestamp - Setup timestamp (unix seconds)
 * @param {string} params.senderPubkey - Sender compressed pubkey hex
 * @param {string} params.receiverPubkey - Receiver compressed pubkey hex
 * @param {number} params.expiryTimestamp - Expiry timestamp (unix seconds)
 * @param {string} channelSecretHex - 32-byte channel secret hex
 * @returns {string} 64-char hex string (channel ID)
 */
export function getChannelId(params, channelSecretHex) {
  const parts = [
    params.mint,
    params.unit,
    params.capacity,
    params.fundingTokenAmount,
    params.keysetId,
    params.inputFeePpk,
    params.maximumAmount,
    params.setupTimestamp,
    params.senderPubkey,
    params.receiverPubkey,
    params.expiryTimestamp,
    channelSecretHex,
  ];
  const paramsString = parts.join("|");
  const hash = sha256(new TextEncoder().encode(paramsString));
  return bytesToHex(hash);
}

// ─── EC Point Arithmetic (internal) ──────────────────────────────

const Point = secp256k1.ProjectivePoint;
const GROUP_ORDER = secp256k1.CURVE.n;

/** Convert Uint8Array to BigInt (big-endian). */
function bytesToBigInt(bytes) {
  return BigInt("0x" + bytesToHex(bytes));
}

/** Convert BigInt to 32-byte Uint8Array (big-endian). */
function bigIntToBytes32(n) {
  const hex = n.toString(16).padStart(64, "0");
  return hexToBytes(hex);
}

// ─── T6: Deterministic Blinding + Funding Outputs ────────────────

/**
 * Derive a blinding scalar for P2BK (Stage 1).
 * Rust source (params.rs — derive_blinding_scalar):
 *   SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry_counter}")
 *   Retries with incrementing retry_counter until valid scalar [1, n-1].
 *
 * @param {Uint8Array} channelSecret - 32-byte channel secret
 * @param {string} channelId - hex channel ID
 * @param {string} context - "sender_stage1", "receiver_stage1", "funding", etc.
 * @returns {Uint8Array} 32-byte blinding scalar
 */
export function deriveBlindingScalar(channelSecret, channelId, context) {
  const prefix = new TextEncoder().encode("Cashu_Spilman_P2BK_v1");
  for (let retryCounter = 0; retryCounter <= 255; retryCounter++) {
    const text = new TextEncoder().encode(
      `${channelId}|${context}|${retryCounter}`,
    );
    const input = new Uint8Array(
      prefix.length + channelSecret.length + text.length,
    );
    input.set(prefix, 0);
    input.set(channelSecret, prefix.length);
    input.set(text, prefix.length + channelSecret.length);

    const hash = sha256(input);
    const hashInt = bytesToBigInt(hash);
    if (hashInt > 0n && hashInt < GROUP_ORDER) {
      return hash;
    }
  }
  throw new Error("Failed to derive valid blinding scalar after 256 attempts");
}

/**
 * Derive a deterministic nonce/secret for a P2BK output.
 * Rust source (params.rs — create_deterministic_output_with_blinding):
 *   SHA256(channel_secret || "{channel_id}|{context}|{amount}|nonce|{index}")
 *
 * @param {Uint8Array} channelSecret - 32-byte channel secret
 * @param {string} channelId - hex channel ID
 * @param {string} context - "funding", "sender", "receiver", etc.
 * @param {number} amount - denomination amount
 * @param {number} index - index within outputs of same amount
 * @returns {string} hex string (deterministic nonce/secret)
 */
export function createDeterministicSecret(
  channelSecret,
  channelId,
  context,
  amount,
  index,
) {
  const text = new TextEncoder().encode(
    `${channelId}|${context}|${amount}|nonce|${index}`,
  );
  const input = new Uint8Array(channelSecret.length + text.length);
  input.set(channelSecret, 0);
  input.set(text, channelSecret.length);
  return bytesToHex(sha256(input));
}

/**
 * Derive a deterministic blinding factor for a P2BK output.
 * Rust source (params.rs — create_deterministic_output_with_blinding):
 *   SHA256(channel_secret || "{channel_id}|{context}|{amount}|blinding|{index}")
 *   Validated as a secp256k1 scalar [1, n-1].
 *
 * @param {Uint8Array} channelSecret - 32-byte channel secret
 * @param {string} channelId - hex channel ID
 * @param {string} context - "funding", "sender", "receiver", etc.
 * @param {number} amount - denomination amount
 * @param {number} index - index within outputs of same amount
 * @returns {Uint8Array} 32-byte blinding factor
 */
export function createDeterministicBlindingFactor(
  channelSecret,
  channelId,
  context,
  amount,
  index,
) {
  const text = new TextEncoder().encode(
    `${channelId}|${context}|${amount}|blinding|${index}`,
  );
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

/**
 * Cashu hash-to-curve: maps arbitrary bytes to a secp256k1 point.
 * NUT-00 spec: SHA256("Secp256k1_HashToCurve_Cashu_" || secret || counter)
 * then try-and-increment to find a valid x-coordinate.
 *
 * @param {Uint8Array} secretBytes - the secret to hash
 * @returns {Point} secp256k1 point (ProjectivePoint)
 */
function hashToCurve(secretBytes) {
  const prefix = new TextEncoder().encode("Secp256k1_HashToCurve_Cashu_");
  for (let counter = 0; counter < 256; counter++) {
    const input = new Uint8Array(prefix.length + secretBytes.length + 1);
    input.set(prefix, 0);
    input.set(secretBytes, prefix.length);
    input[prefix.length + secretBytes.length] = counter;
    const hash = sha256(input);
    const xHex = bytesToHex(hash);
    // Try even y (02 prefix)
    try {
      return Point.fromHex("02" + xHex);
    } catch {
      // x not on curve with even y, try odd y
    }
    // Try odd y (03 prefix)
    try {
      return Point.fromHex("03" + xHex);
    } catch {
      // x not on curve at all, increment counter
    }
  }
  throw new Error("hashToCurve: failed to find valid point after 256 attempts");
}

/**
 * Blind a message using standard Cashu DHKE.
 * B_ = hash_to_curve(secret) + r * G
 * where r is the blinding scalar and G is the secp256k1 generator.
 *
 * The mintPubkeyForAmount parameter is included for API completeness
 * but is not used in the standard Cashu blinding step (NUT-00).
 *
 * @param {Uint8Array} secretBytes - the secret to blind
 * @param {Uint8Array} blindingScalar - r scalar (32 bytes)
 * @param {string} _mintPubkeyForAmount - mint pubkey for denomination (unused in blinding)
 * @returns {string} B_ compressed point hex (33 bytes / 66 hex chars)
 */
export function blindMessage(secretBytes, blindingScalar, _mintPubkeyForAmount) {
  const Y = hashToCurve(secretBytes);
  const rPoint = Point.BASE.multiply(blindingScalar);
  const B_ = Y.add(rPoint);
  return bytesToHex(B_.toRawBytes(true));
}

/**
 * Split a target amount into denomination amounts (powers of 2).
 * Largest denomination first. Multiple outputs of the same denomination
 * are used when the target exceeds a single maxPerOutput chunk.
 *
 * @param {number} targetAmount - total amount to split (sat)
 * @param {number} maxPerOutput - maximum value per single output
 * @returns {number[]} array of denomination amounts (largest first)
 */
export function getDenominationAmounts(targetAmount, maxPerOutput) {
  if (targetAmount === 0) return [];

  const amounts = [];
  let remaining = targetAmount;

  // Find the largest power of 2 <= maxPerOutput
  let denom = 1;
  while (denom * 2 <= maxPerOutput) {
    denom *= 2;
  }

  // Greedy: use largest denomination, allow multiple of same
  while (remaining > 0 && denom >= 1) {
    if (denom <= remaining) {
      amounts.push(denom);
      remaining -= denom;
    } else {
      denom = Math.floor(denom / 2);
    }
  }

  if (remaining > 0) {
    throw new Error(
      `Cannot represent remaining ${remaining} sat with denominations up to ${maxPerOutput}`,
    );
  }

  return amounts;
}

/**
 * Create a deterministic blinded output for a specific denomination.
 * Combines secret derivation, blinding factor derivation, and message blinding.
 *
 * @param {Uint8Array} channelSecret - 32-byte channel secret
 * @param {string} channelId - hex channel ID
 * @param {string} context - "funding", "sender", "receiver", etc.
 * @param {number} amount - denomination amount
 * @param {number} index - index within outputs of same amount
 * @returns {{ secret: string, blindingFactor: string, B_: string, amount: number }}
 */
export function createDeterministicOutput(
  channelSecret,
  channelId,
  context,
  amount,
  index,
) {
  const secret = createDeterministicSecret(
    channelSecret,
    channelId,
    context,
    amount,
    index,
  );
  const blindingFactor = createDeterministicBlindingFactor(
    channelSecret,
    channelId,
    context,
    amount,
    index,
  );
  const secretBytes = new TextEncoder().encode(secret);
  const B_ = blindMessage(secretBytes, blindingFactor, "");
  return {
    secret,
    blindingFactor: bytesToHex(blindingFactor),
    B_,
    amount,
  };
}

// ─── T7: Unblinding + Proof Construction ─────────────────────────

/**
 * Unblind a blind signature to get the proof commitment.
 * NUT-00: C = C_ - r * K_a (elliptic curve point subtraction)
 * where C_ is the blind signature point, r is the blinding factor,
 * and K_a is the mint's pubkey for this denomination.
 *
 * @param {string} blindSignatureHex - C_ compressed point hex
 * @param {Uint8Array} blindingScalar - r scalar (32 bytes)
 * @param {string} mintPubkeyHex - K_a compressed point hex for this denomination
 * @returns {string} C compressed point hex
 */
export function unblindSignature(
  blindSignatureHex,
  blindingScalar,
  mintPubkeyHex,
) {
  const C_ = Point.fromHex(blindSignatureHex);
  const K_a = Point.fromHex(mintPubkeyHex);
  // r * K_a
  const rK_a = K_a.multiply(blindingScalar);
  // C = C_ - r * K_a = C_ + (-r * K_a)
  const C = C_.add(rK_a.negate());
  return bytesToHex(C.toRawBytes(true));
}

/**
 * Construct proofs from blind signatures, secrets, and blinding factors.
 * Mirrors cashu::dhke::construct_proofs — for each blind signature,
 * unblinds to get C and assembles a proof object.
 *
 * @param {Array<{amount: number, C_: string}>} blindSignatures - blind sigs from mint
 * @param {Array<{secret: string, blinding_factor: string, amount: number}>} secretsWithBlinding
 * @param {string} keysetId - keyset ID for proofs
 * @param {Object<number, string>} mintKeys - map of amount → mint pubkey hex
 * @returns {Array<{amount: number, secret: string, C: string, id: string}>}
 */
export function constructProofs(
  blindSignatures,
  secretsWithBlinding,
  keysetId,
  mintKeys,
) {
  if (blindSignatures.length !== secretsWithBlinding.length) {
    throw new Error(
      `Signature count ${blindSignatures.length} != secret count ${secretsWithBlinding.length}`,
    );
  }

  return blindSignatures.map((sig, i) => {
    const swb = secretsWithBlinding[i];
    const mintPubkeyHex = mintKeys[String(swb.amount)];
    if (!mintPubkeyHex) {
      throw new Error(`No mint key for amount ${swb.amount}`);
    }
    const blindingScalar =
      typeof swb.blinding_factor === "string"
        ? hexToBytes(swb.blinding_factor)
        : swb.blinding_factor;

    const C = unblindSignature(sig.C_, blindingScalar, mintPubkeyHex);

    return {
      amount: swb.amount,
      secret: swb.secret,
      C,
      id: keysetId,
    };
  });
}

/**
 * Verify that a set of proofs is well-formed and sums to the expected total.
 * Checks: each proof has required fields, C is a valid curve point,
 * and the total amount matches expectedAmounts.
 *
 * @param {Object} _channelParams - channel parameters (reserved for future checks)
 * @param {Array<{amount: number, secret: string, C: string, id: string}>} proofs
 * @param {number} expectedTotalAmount - expected sum of proof amounts
 * @returns {boolean} true if all proofs are valid
 */
export function verifyValidChannel(
  _channelParams,
  proofs,
  expectedTotalAmount,
) {
  let total = 0;
  for (const proof of proofs) {
    if (!proof.amount || !proof.secret || !proof.C || !proof.id) {
      return false;
    }
    try {
      Point.fromHex(proof.C);
    } catch {
      return false;
    }
    total += proof.amount;
  }
  return total === expectedTotalAmount;
}

// ─── T8: Schnorr-Signed Balance Update ───────────────────────────

/**
 * Create a Schnorr-signed balance update.
 *
 * The signing key is Alice's private key tweaked with a channel-secret-derived
 * blinding scalar (for "sender_stage1" context). BIP-340 parity is handled:
 * if Alice's pubkey has odd Y, the secret is negated before adding the tweak.
 *
 * The message hash is computed as:
 *   SHA256(channel_id_hex + "|" + balance_to_receiver)
 * (Simplified — the full Rust implementation hashes the serialized swap request
 * via sig_all_message_hash, but that requires constructing commitment outputs.)
 *
 * @param {Object} _params - Channel parameters (reserved for future expansion)
 * @param {string} aliceSecretHex - Alice's private key hex
 * @param {string} channelSecretHex - Channel secret hex
 * @param {string} channelIdHex - Channel ID hex
 * @param {number} balanceToReceiver - Amount owed to receiver (sat)
 * @returns {{ messageHex: string, signatureHex: string, tweakedPubHex: string }}
 */
export function createSignedBalanceUpdate(
  _params,
  aliceSecretHex,
  channelSecretHex,
  channelIdHex,
  balanceToReceiver,
) {
  const channelSecret = hexToBytes(channelSecretHex);

  // 1. Derive the tweak scalar for "sender_stage1"
  const tweakScalar = deriveBlindingScalar(
    channelSecret,
    channelIdHex,
    "sender_stage1",
  );
  const tweakBigInt = bytesToBigInt(tweakScalar);

  // 2. Handle BIP-340 parity on Alice's key
  const aliceSecret = hexToBytes(aliceSecretHex);
  const alicePubCompressed = secp256k1.getPublicKey(aliceSecret, true);
  const parityIsOdd = alicePubCompressed[0] === 0x03;

  let effectiveSecretBigInt = bytesToBigInt(aliceSecret);
  if (parityIsOdd) {
    effectiveSecretBigInt = GROUP_ORDER - effectiveSecretBigInt;
  }

  // 3. Add tweak: tweaked = (effective_secret + tweak) mod n
  const tweakedBigInt = (effectiveSecretBigInt + tweakBigInt) % GROUP_ORDER;
  const tweakedBytes = bigIntToBytes32(tweakedBigInt);

  // 4. Compute message hash
  const messageInput = new TextEncoder().encode(
    `${channelIdHex}|${balanceToReceiver}`,
  );
  const messageHash = sha256(messageInput);

  // 5. Sign with Schnorr using the tweaked key
  const signature = schnorr.sign(messageHash, tweakedBytes);

  // 6. Compute the tweaked x-only public key
  const tweakedPub = schnorr.getPublicKey(tweakedBytes);

  return {
    messageHex: bytesToHex(messageHash),
    signatureHex: bytesToHex(signature),
    tweakedPubHex: bytesToHex(tweakedPub),
  };
}

/**
 * Verify a Schnorr balance update signature.
 *
 * @param {string} messageHex - 32-byte message hash hex
 * @param {string} signatureHex - 64-byte Schnorr signature hex
 * @param {string} tweakedPubHex - 32-byte x-only tweaked pubkey hex
 * @returns {boolean} true if signature is valid
 */
export function verifyBalanceUpdate(messageHex, signatureHex, tweakedPubHex) {
  return schnorr.verify(
    hexToBytes(signatureHex),
    hexToBytes(messageHex),
    hexToBytes(tweakedPubHex),
  );
}
