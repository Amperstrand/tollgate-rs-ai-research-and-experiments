// test-vectors-cdk.js — Validate Rust-captured test vectors against cdk-wasm
//
// Uses the cdk-wasm WASM module (compiled from the same Rust crate that
// generated the test vectors) to validate every captured intermediate value.
// Since cdk-wasm IS the Rust code compiled to WASM, all checks should pass
// if the WASM loaded correctly and the test-vectors.json is fresh.
//
// Usage: window.runCdkVectors() in browser console

import { initCdkWasm, getCdkWasm } from "./cdk-wasm-bridge.js";

export async function runCdkVectors() {
  const failures = [];
  let passed = 0;
  let failed = 0;

  // -- Fetch vectors ----------------------------------------------------------
  let v;
  try {
    const resp = await fetch("../test-vectors.json?t=" + Date.now());
    if (!resp.ok) {
      return {
        pass: true,
        skipped: true,
        skipReason:
          "test-vectors.json not found. Generate with: cargo test -p tollgate-net --test cdk_spilman_test_vectors --features spilman -- --ignored --nocapture",
        passed: 0,
        failed: 0,
        failures: [],
      };
    }
    v = await resp.json();
  } catch (err) {
    return {
      pass: false,
      passed: 0,
      failed: 1,
      failures: [{ check: "fetch/parse test-vectors.json", expected: "valid JSON", actual: err.message }],
    };
  }

  // -- Initialize WASM --------------------------------------------------------
  let wasm;
  try {
    await initCdkWasm();
    wasm = getCdkWasm();
  } catch (err) {
    return {
      pass: false,
      passed: 0,
      failed: 1,
      failures: [{ check: "cdk-wasm init", expected: "WASM loaded", actual: err.message }],
    };
  }

  function check(label, actual, expected) {
    if (actual === expected) {
      passed++;
    } else {
      failed++;
      failures.push({ check: label, expected: String(expected).slice(0, 64), actual: String(actual).slice(0, 64) });
    }
  }

  // -- 1. ECDH shared secret --------------------------------------------------
  // cdk-wasm: compute_channel_secret(my_secret_hex, their_pubkey_hex) -> hex
  // Returns the domain-separated channel secret (not raw ECDH x-coordinate).
  try {
    const channelSecret = (wasm.compute_channel_secret)(v.alice_seed_hex, v.charlie_pubkey_hex);
    check("channel secret (cdk-wasm)", channelSecret, v.channel_secret_hex);
  } catch (err) {
    failed++;
    failures.push({ check: "ECDH shared secret", expected: v.ecdh_shared_secret_hex?.slice(0, 32) + "...", actual: `ERROR: ${err.message}` });
  }

  // -- 2. Channel ID ----------------------------------------------------------
  // cdk-wasm: channel_parameters_get_channel_id(params_json, channel_secret_hex, keyset_info_json) -> hex
  // Second arg is the domain-separated channel secret, NOT raw ECDH.
  try {
    const channelId = wasm.channel_parameters_get_channel_id(
      v.params_json,
      v.channel_secret_hex,
      v.keyset_info_json,
    );
    check("channel ID (cdk-wasm)", channelId, v.channel_id_hex);
  } catch (err) {
    failed++;
    failures.push({ check: "channel ID", expected: v.channel_id_hex?.slice(0, 32) + "...", actual: `ERROR: ${err.message}` });
  }

  // -- 3. Funding outputs ------------------------------------------------------
  // cdk-wasm: create_funding_outputs(params_json, my_secret_hex, keyset_info_json) -> JSON
  //   Returns: { funding_token_nominal, blinded_messages, secrets_with_blinding }
  //   blinded_messages[i] = { amount, B_, secret, blinding_factor_r }
  let fundingOutput;
  try {
    const fundingJson = wasm.create_funding_outputs(
      v.params_json,
      v.alice_seed_hex,
      v.keyset_info_json,
    );
    fundingOutput = JSON.parse(fundingJson);

    check("funding output nominal amount", String(fundingOutput.funding_token_nominal), String(v.funding_amount_sat));

    check(
      "funding blinded_messages count",
      String(fundingOutput.blinded_messages.length),
      String(v.funding_blinded_messages_sender_stage1.length),
    );

    const expectedBms = v.funding_blinded_messages_sender_stage1;
    for (let i = 0; i < expectedBms.length; i++) {
      const got = fundingOutput.blinded_messages[i];
      const exp = expectedBms[i];

      check(`blinded_message[${i}] amount`, String(got.amount), String(exp.amount));
      check(`blinded_message[${i}] B_`, got.B_, exp.B_);
    }

    for (let i = 0; i < expectedBms.length; i++) {
      const swb = fundingOutput.secrets_with_blinding[i];
      const exp = expectedBms[i];
      check(`secrets_with_blinding[${i}] secret`, swb.secret, exp.secret);
      check(`secrets_with_blinding[${i}] blinding_factor`, swb.blinding_factor, exp.blinding_factor_r);
      check(`secrets_with_blinding[${i}] amount`, String(swb.amount), String(exp.amount));
    }
  } catch (err) {
    failed++;
    failures.push({ check: "create_funding_outputs", expected: "success", actual: `ERROR: ${err.message}` });
  }

  // -- 4. Construct proofs -----------------------------------------------------
  // cdk-wasm: construct_proofs(blind_signatures_json, secrets_with_blinding_json, keyset_info_json) -> JSON
  try {
    if (fundingOutput && Array.isArray(v.funding_blind_signatures_sender_stage1)) {
      const proofsJson = wasm.construct_proofs(
        JSON.stringify(v.funding_blind_signatures_sender_stage1),
        JSON.stringify(fundingOutput.secrets_with_blinding),
        v.keyset_info_json,
      );
      const proofs = JSON.parse(proofsJson);

      check("proof count", String(proofs.length), String(v.constructed_proofs_sender_stage1.length));

      const expectedProofs = v.constructed_proofs_sender_stage1;
      for (let i = 0; i < expectedProofs.length; i++) {
        check(`proof[${i}] amount`, String(proofs[i].amount), String(expectedProofs[i].amount));
        check(`proof[${i}] secret`, proofs[i].secret, expectedProofs[i].secret);
        check(`proof[${i}] C`, proofs[i].C, expectedProofs[i].C);
      }
    }
  } catch (err) {
    failed++;
    failures.push({ check: "construct_proofs", expected: "success", actual: `ERROR: ${err.message}` });
  }

  // -- 5. Signed balance update ------------------------------------------------
  // cdk-wasm: spilman_channel_sender_create_signed_balance_update(
  //   params_json, keyset_info_json, alice_secret_hex, funding_proofs_json, charlie_balance
// ) -> JSON { channel_id, amount, signature }
  try {
    if (v.signed_balance_update) {
      const sbu = v.signed_balance_update;
      const balResultJson = wasm.spilman_channel_sender_create_signed_balance_update(
        v.params_json,
        v.keyset_info_json,
        v.alice_seed_hex,
        v.funding_proofs_json,
        BigInt(sbu.amount_to_charlie),
      );
      const balResult = JSON.parse(balResultJson);

      check("balance update channel_id", balResult.channel_id, v.channel_id_hex);
      check("balance update amount", String(balResult.amount), String(sbu.amount_to_charlie));

      // Signature uses a random nonce, so we verify it instead of comparing
      const sigValid = wasm.verify_balance_update_signature(
        v.params_json,
        v.channel_secret_hex,
        v.funding_proofs_json,
        v.keyset_info_json,
        balResult.channel_id,
        BigInt(balResult.amount),
        balResult.signature,
      );
      check("balance update signature (verified)", String(sigValid), "true");
    }
  } catch (err) {
    failed++;
    failures.push({ check: "signed balance update", expected: "success", actual: `ERROR: ${err.message}` });
  }

  // -- 6. Verify channel -------------------------------------------------------
  // cdk-wasm: verify_channel(params_json, shared_secret_hex, funding_proofs_json, keyset_info_json)
  try {
    const verifyJson = wasm.verify_channel(
      v.params_json,
      v.channel_secret_hex,
      v.funding_proofs_json,
      v.keyset_info_json,
    );
    const verifyResult = JSON.parse(verifyJson);
    check("verify_channel result", String(verifyResult.valid), "true");
  } catch (err) {
    failed++;
    failures.push({ check: "verify_channel", expected: "valid", actual: `ERROR: ${err.message}` });
  }

  // -- 7. Verify balance update signature --------------------------------------
  // cdk-wasm: verify_balance_update_signature(
  //   params_json, shared_secret_hex, funding_proofs_json, keyset_info_json,
  //   channel_id, balance, signature
  // ) -> boolean
  try {
    if (v.signed_balance_update) {
      const sbu = v.signed_balance_update;
      const sigValid = wasm.verify_balance_update_signature(
        v.params_json,
        v.channel_secret_hex,
        v.funding_proofs_json,
        v.keyset_info_json,
        v.channel_id_hex,
        BigInt(sbu.amount_to_charlie),
        sbu.signature_hex,
      );
      check("verify_balance_update_signature", String(sigValid), "true");
    }
  } catch (err) {
    failed++;
    failures.push({ check: "verify_balance_update_signature", expected: "true", actual: `ERROR: ${err.message}` });
  }

  return {
    pass: failed === 0 && failures.length === 0,
    passed,
    failed,
    failures,
  };
}
