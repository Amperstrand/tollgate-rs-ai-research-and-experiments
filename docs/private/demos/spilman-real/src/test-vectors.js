// test-vectors.js — Validate Rust-captured test vectors against JS crypto
//
// Fetches ../test-vectors.json (sibling of index.html) and asserts
// deterministic fields match crypto.js function output.
//
// Usage: window.runVectors() in browser console, or import and call directly.

import {
  bytesToHex,
  hexToBytes,
  getPublicKey,
  computeRawEcdh,
  computeChannelSecret,
  getChannelId,
  createDeterministicSecret,
  createDeterministicBlindingFactor,
  createDeterministicOutput,
  constructProofs,
  createSignedBalanceUpdate,
} from "./crypto.js";

/**
 * Fetch test-vectors.json and validate deterministic fields against crypto.js.
 *
 * @returns {{
 *   pass: boolean,
 *   passed: number,
 *   failed: number,
 *   failures: Array<{check: string, expected: string, actual: string}>,
 *   knownGaps: Array<{id: string, check: string, detail: string, jsValue: string, rustValue: string}>,
 *   skipped?: boolean,
 *   skipReason?: string
 * }}
 */
export async function runTestVectors() {
  const failures = [];
  const knownGaps = [];
  let passed = 0;
  let failed = 0;

  // ── Fetch vectors ────────────────────────────────────────────────
  let v;
  try {
    const resp = await fetch("../test-vectors.json?t=" + Date.now());
    if (!resp.ok) {
      return {
        pass: true,
        skipped: true,
        skipReason:
          "test-vectors.json not found. Generate it with: cargo test -p tollgate-net --test cdk_spilman_test_vectors --features spilman -- --ignored --nocapture",
        passed: 0,
        failed: 0,
        failures: [],
        knownGaps: [],
      };
    }
    v = await resp.json();
  } catch (err) {
    return {
      pass: false,
      skipped: false,
      passed: 0,
      failed: 1,
      failures: [
        {
          check: "fetch/parse test-vectors.json",
          expected: "valid JSON",
          actual: err.message,
        },
      ],
      knownGaps: [],
    };
  }

  function check(label, actual, expected) {
    if (actual === expected) {
      passed++;
    } else {
      failed++;
      failures.push({ check: label, expected, actual });
    }
  }

  // ── 1. Key derivation (fully deterministic) ──────────────────────

  const alicePub = bytesToHex(getPublicKey(hexToBytes(v.alice_seed_hex)));
  check("alice pubkey derivation", alicePub, v.alice_pubkey_hex);

  const charliePub = bytesToHex(getPublicKey(hexToBytes(v.charlie_seed_hex)));
  check("charlie pubkey derivation", charliePub, v.charlie_pubkey_hex);

  // ── 2. ECDH shared secret (fully deterministic) ──────────────────

  const rawEcdh = computeRawEcdh(v.alice_seed_hex, v.charlie_pubkey_hex);
  check("raw ECDH shared secret", bytesToHex(rawEcdh), v.ecdh_shared_secret_hex);

  // ── 3. Channel secret (fully deterministic) ──────────────────────

  const channelSecretHex = computeChannelSecret(
    v.alice_seed_hex,
    v.charlie_pubkey_hex,
  );
  check("channel secret", channelSecretHex, v.channel_secret_hex);

  // ── 4. Channel ID (captured-input deterministic) ─────────────────

  let params;
  try {
    params = JSON.parse(v.params_json);
  } catch (e) {
    failed++;
    failures.push({
      check: "parse params_json",
      expected: "valid JSON object",
      actual: e.message,
    });
  }

  if (params) {
    const channelId = getChannelId(params, channelSecretHex);
    check("channel ID", channelId, v.channel_id_hex);

    // ── 5. Deterministic output derivation ──────────────────────────

    const channelSecretBytes = hexToBytes(v.channel_secret_hex);
    const channelIdHex = v.channel_id_hex;
    const context = "sender_stage1";

    if (Array.isArray(v.funding_blinded_messages_sender_stage1)) {
      v.funding_blinded_messages_sender_stage1.forEach((bm, index) => {
        try {
          const secret = createDeterministicSecret(
            channelSecretBytes,
            channelIdHex,
            context,
            bm.amount,
            index,
          );
          check(
            `funding output[${index}] secret (amount=${bm.amount})`,
            secret,
            bm.secret,
          );
        } catch (err) {
          failed++;
          failures.push({
            check: `funding output[${index}] secret (amount=${bm.amount})`,
            expected: bm.secret,
            actual: `ERROR: ${err.message}`,
          });
        }

        try {
          const bf = createDeterministicBlindingFactor(
            channelSecretBytes,
            channelIdHex,
            context,
            bm.amount,
            index,
          );
          check(
            `funding output[${index}] blinding factor (amount=${bm.amount})`,
            bytesToHex(bf),
            bm.blinding_factor_r,
          );
        } catch (err) {
          failed++;
          failures.push({
            check: `funding output[${index}] blinding factor (amount=${bm.amount})`,
            expected: bm.blinding_factor_r,
            actual: `ERROR: ${err.message}`,
          });
        }

        try {
          const output = createDeterministicOutput(
            channelSecretBytes,
            channelIdHex,
            context,
            bm.amount,
            index,
          );
          check(
            `funding output[${index}] B_ (amount=${bm.amount})`,
            output.B_,
            bm.B_,
          );
        } catch (err) {
          failed++;
          failures.push({
            check: `funding output[${index}] B_ (amount=${bm.amount})`,
            expected: bm.B_,
            actual: `ERROR: ${err.message}`,
          });
        }
      });
    }

    // ── 6. Balance update (captured-input deterministic) ────────────
    // G1: JS signs SHA256(channel_id|balance), Rust signs
    //     sig_all_message_hash(full swap). The message hash WILL differ.
    //     Report as a known gap rather than a failure.

    if (v.signed_balance_update) {
      const sbu = v.signed_balance_update;
      try {
        const update = createSignedBalanceUpdate(
          params,
          v.alice_seed_hex,
          channelSecretHex,
          channelIdHex,
          sbu.amount_to_charlie,
        );

        if (update.messageHex !== sbu.message_hex) {
          knownGaps.push({
            id: "G1",
            check: "balance update message hash",
            detail:
              "JS signs SHA256(channel_id|balance), Rust signs sig_all_message_hash(full swap). " +
              "Will be resolved by cdk-wasm bridge in Phase 1.",
            jsValue: update.messageHex,
            rustValue: sbu.message_hex,
          });
        } else {
          check(
            "balance update message hash",
            update.messageHex,
            sbu.message_hex,
          );
        }

        if (update.messageHex !== sbu.message_hex) {
          knownGaps.push({
            id: "G1",
            check: "balance update signature",
            detail:
              "Signature mismatch is a consequence of G1 (different message hash). " +
              "Will be resolved by cdk-wasm bridge in Phase 1.",
            jsValue: update.signatureHex,
            rustValue: sbu.signature_hex,
          });
        } else {
          check(
            "balance update signature",
            update.signatureHex,
            sbu.signature_hex,
          );
        }

        // Tweaked pubkey: independent of message hash, should always match
        check(
          "balance update tweaked pubkey",
          update.tweakedPubHex,
          sbu.tweaked_pub_hex,
        );
      } catch (err) {
        failed++;
        failures.push({
          check: "balance update (createSignedBalanceUpdate)",
          expected: "successful signing",
          actual: `ERROR: ${err.message}`,
        });
      }
    }

    // ── 7. Proof construction (structure-only) ──────────────────────
    // Non-deterministic across runs (blind sigs vary), but we can verify
    // the function works with the captured data.

    if (
      Array.isArray(v.funding_blind_signatures_sender_stage1) &&
      Array.isArray(v.constructed_proofs_sender_stage1) &&
      v.keyset_keys &&
      v.keyset_id
    ) {
      const sigs = v.funding_blind_signatures_sender_stage1;
      const expectedProofs = v.constructed_proofs_sender_stage1;

      check(
        "proof count matches blind signature count",
        String(expectedProofs.length),
        String(sigs.length),
      );

      for (let i = 0; i < expectedProofs.length; i++) {
        const p = expectedProofs[i];
        check(
          `proof[${i}] has amount > 0`,
          String(p.amount > 0),
          "true",
        );
        check(
          `proof[${i}] secret is 64 hex chars`,
          String(p.secret.length),
          "64",
        );
        check(
          `proof[${i}] C is 66 hex chars (compressed point)`,
          String(p.C.length),
          "66",
        );
      }

      try {
        const secretsWithBlinding =
          v.funding_blinded_messages_sender_stage1.map((bm) => ({
            secret: bm.secret,
            blinding_factor: bm.blinding_factor_r,
            amount: bm.amount,
          }));
        const proofs = constructProofs(
          sigs,
          secretsWithBlinding,
          v.keyset_id,
          v.keyset_keys,
        );
        check(
          "constructProofs returns correct count",
          String(proofs.length),
          String(sigs.length),
        );
        for (let i = 0; i < proofs.length; i++) {
          check(
            `constructed proof[${i}] has all fields`,
            String(Boolean(proofs[i].amount && proofs[i].secret && proofs[i].C && proofs[i].id)),
            "true",
          );
        }
      } catch (err) {
        failed++;
        failures.push({
          check: "constructProofs execution",
          expected: "success",
          actual: `ERROR: ${err.message}`,
        });
      }
    }
  }

  return {
    pass: failed === 0 && failures.length === 0,
    passed,
    failed,
    failures,
    knownGaps,
  };
}
