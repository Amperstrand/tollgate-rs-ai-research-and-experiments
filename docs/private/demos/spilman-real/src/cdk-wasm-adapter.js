// cdk-wasm-adapter.js — Format conversion + SIG_ALL witness helpers

import { getCdkWasm } from "./cdk-wasm-bridge.js";
import { sha256, bytesToHex, hexToBytes } from "./crypto.js";

export function toParamsJson(params) {
  return JSON.stringify({
    capacity: params.capacity,
    expiry_timestamp: params.expiryTimestamp,
    funding_token_amount: params.fundingTokenAmount,
    input_fee_ppk: params.inputFeePpk,
    keyset_id: params.keysetId,
    maximum_amount: params.maximumAmount,
    mint: params.mint,
    receiver_pubkey: params.receiverPubkey,
    sender_pubkey: params.senderPubkey,
    setup_timestamp: params.setupTimestamp,
    unit: params.unit,
  });
}

export function toKeysetInfoJson(keysetId, keys, inputFeePpk, unit = "sat") {
  return JSON.stringify({ inputFeePpk, keys, keysetId, unit });
}

export function wasm() {
  return getCdkWasm();
}

// P2BK blinding scalar derivation (mirrors cdk-spilman params.rs:539-562)
// SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry_counter}")
export function deriveBlindingScalar(channelSecretHex, channelId, context) {
  const secretBytes = hexToBytes(channelSecretHex);
  for (let retry = 0; retry <= 255; retry++) {
    const text = `${channelId}|${context}|${retry}`;
    const input = new Uint8Array([
      ...new TextEncoder().encode("Cashu_Spilman_P2BK_v1"),
      ...secretBytes,
      ...new TextEncoder().encode(text),
    ]);
    const hash = sha256(input);
    const scalarHex = bytesToHex(hash);
    const scalarBigInt = BigInt("0x" + scalarHex);
    const n = BigInt("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");
    if (scalarBigInt > 0n && scalarBigInt < n) {
      return scalarHex;
    }
  }
  throw new Error("Failed to derive valid blinding scalar after 256 attempts");
}

// SIG_ALL message for a SwapRequest (mirrors cashu crate nut03.rs:101-119)
// msg = secret_0 || C_0 || ... || secret_n || C_n || amount_0 || B_0 || ... || amount_m || B_m
export function computeSigAllMessage(inputs, outputs) {
  let msg = "";
  for (const proof of inputs) {
    msg += proof.secret;
    msg += proof.C;
  }
  for (const output of outputs) {
    msg += output.amount.toString();
    msg += output.B_;
  }
  return msg;
}

export function sha256Hex(input) {
  if (typeof input === "string") {
    input = new TextEncoder().encode(input);
  }
  return bytesToHex(sha256(input));
}
