//! cdk-spilman Test Vector Capture
//!
//! Mirrors `cdk_spilman_bridge_spike.rs` but uses deterministic keys and captures
//! all intermediate crypto values to `docs/private/demos/spilman-real/test-vectors.json`
//! for JS-side validation.
//!
//! Run with:
//!   cargo test -p tollgate-net --test cdk_spilman_test_vectors \
//!     --features spilman -- --ignored --nocapture

mod common;

#[cfg(feature = "spilman")]
use {
    cashu::mint_url::MintUrl,
    cashu::nuts::Token as CashuToken,
    cashu::nuts::{CurrencyUnit, Proof as CashuProof, PublicKey, SecretKey},
    cdk::secp256k1::{self, ecdh::SharedSecret, Parity, Scalar, Secp256k1},
    cdk_spilman::{
        channel_parameters_get_channel_id, complete_funding_swap, compute_channel_from_token,
        compute_channel_secret_from_hex, create_funding_swap, create_signed_balance_update,
        create_unsigned_balance_update,
    },
    std::path::PathBuf,
    std::str::FromStr,
    std::time::{SystemTime, UNIX_EPOCH},
    tollgate_net::cdk_wallet::CdkWallet,
    tollgate_net::spilman_wallet::fetch_active_keyset_info,
};

#[cfg(feature = "spilman")]
const MINT_URL: &str = "https://testnut.cashu.exchange";

#[cfg(feature = "spilman")]
const ALICE_SEED_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[cfg(feature = "spilman")]
const CHARLIE_SEED_HEX: &str = "2222222222222222222222222222222222222222222222222222222222222222";

#[cfg(feature = "spilman")]
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Helper: POST JSON to a URL and return the response body.
#[cfg(feature = "spilman")]
async fn http_post(url: &str, body: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!("POST {url} → {status}: {text}"));
    }
    Ok(text)
}

/// Compute the raw ECDH shared secret (before domain separation) as hex.
/// The channel_secret in cdk-spilman is SHA256("Cashu_Spilman_channel_secret_v1" || raw_ecdh).
#[cfg(feature = "spilman")]
fn compute_raw_ecdh_hex(my_secret: &SecretKey, their_pubkey: &PublicKey) -> String {
    let raw = SharedSecret::new(their_pubkey, my_secret);
    hex_encode(&raw.secret_bytes())
}

/// Hex-encode bytes (inlined to avoid importing cashu::util::hex).
#[cfg(feature = "spilman")]
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Resolve the output path for test-vectors.json.
/// Writes to `docs/private/demos/spilman-real/test-vectors.json` relative to
/// the workspace root.
#[cfg(feature = "spilman")]
fn output_path() -> PathBuf {
    // Start from CARGO_MANIFEST_DIR (crates/tollgate-net/) and go up to workspace root.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("tollgate-net has parent")
        .parent()
        .expect("crates has parent")
        .join("docs/private/demos/spilman-real/test-vectors.json")
}

#[cfg(feature = "spilman")]
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires network access to testnut.cashu.exchange"]
async fn capture_spilman_test_vectors() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=debug,info")
        .with_test_writer()
        .try_init();

    tracing::info!("=== Test Vector Capture Start ===");

    // ─── Step 1: Deterministic keys ───
    let alice_secret = SecretKey::from_slice(&hex_decode(ALICE_SEED_HEX).expect("alice seed hex"))
        .expect("alice secret from slice");
    let charlie_secret =
        SecretKey::from_slice(&hex_decode(CHARLIE_SEED_HEX).expect("charlie seed hex"))
            .expect("charlie secret from slice");

    let alice_pubkey = alice_secret.public_key();
    let charlie_pubkey = charlie_secret.public_key();
    let alice_pubkey_hex = alice_pubkey.to_hex();
    let charlie_pubkey_hex = charlie_pubkey.to_hex();

    tracing::info!("Alice pubkey:  {alice_pubkey_hex}");
    tracing::info!("Charlie pubkey: {charlie_pubkey_hex}");

    // ─── Step 2: Compute channel secrets ───
    // Raw ECDH (before domain separation)
    let ecdh_shared_secret_hex =
        compute_raw_ecdh_hex(&alice_secret, &charlie_pubkey);

    // Domain-separated channel secret (what cdk-spilman uses internally)
    let channel_secret_hex =
        compute_channel_secret_from_hex(&alice_secret.to_secret_hex(), &charlie_pubkey_hex)
            .expect("channel_secret from alice→charlie");

    tracing::info!("Raw ECDH:     {ecdh_shared_secret_hex}");
    tracing::info!("Channel secret: {channel_secret_hex}");

    // ─── Step 3: Mint tokens from testnut ───
    tracing::info!("Minting tokens from testnut");
    let wallet = CdkWallet::new(MINT_URL, rand::random())
        .await
        .expect("CdkWallet init");
    wallet.mint_test_tokens(2000).await.expect("mint 2000 sat");
    let bal = wallet.total_balance().await.expect("balance check");
    tracing::info!("Wallet balance: {bal} sat");
    assert!(bal >= 1000, "need >= 1000 sat, got {bal}");

    // Extract proofs and build a CashuToken
    let proofs_json = wallet.unspent_proofs_json().await.expect("get proofs");
    let all_proofs: Vec<CashuProof> = serde_json::from_str(&proofs_json).expect("parse proofs");
    tracing::info!("Got {} unspent proofs", all_proofs.len());

    let mut selected_proofs = Vec::new();
    let mut selected_total = 0u64;
    for proof in &all_proofs {
        if selected_total >= 1000 {
            break;
        }
        selected_proofs.push(proof.clone());
        selected_total += u64::from(proof.amount);
    }
    tracing::info!(
        "Selected {selected_total} sat from {} proofs",
        selected_proofs.len()
    );
    assert!(
        selected_total >= 1000,
        "need >= 1000 sat, got {selected_total}"
    );

    let mint_url = MintUrl::from_str(MINT_URL).expect("parse mint URL");
    let token = CashuToken::new(mint_url, selected_proofs, None, CurrencyUnit::Sat);
    let token_str = token.to_string();
    tracing::info!("Token: {} bytes", token_str.len());

    // ─── Step 4: Fetch keyset info ───
    tracing::info!("Fetching keyset info");
    let (keyset_info_json, keyset_info) = fetch_active_keyset_info(MINT_URL)
        .await
        .expect("fetch keyset info");
    let keyset_id = keyset_info.keyset_id.to_string();
    let keyset_input_fee_ppk = keyset_info.input_fee_ppk;
    tracing::info!("Keyset: id={keyset_id} fee_ppk={keyset_input_fee_ppk}");

    // Extract keyset keys as {"amount": "pubkey_hex", ...}
    let keyset_keys_value: serde_json::Value = {
        let raw: serde_json::Value =
            serde_json::from_str(&keyset_info_json).expect("parse keyset_info_json");
        raw["keys"].clone()
    };

    // ─── Step 5: Compute channel parameters from token ───
    tracing::info!("Computing channel params from token");
    let expiry = now_secs() + 3600;
    let max_amount_per_output: u64 = 64;

    let channel_result_json = compute_channel_from_token(
        &token_str,
        &charlie_pubkey_hex, // receiver
        &alice_pubkey_hex,   // sender
        &channel_secret_hex,
        expiry,
        &keyset_info_json,
        max_amount_per_output,
    )
    .expect("compute_channel_from_token");
    let channel_result: serde_json::Value =
        serde_json::from_str(&channel_result_json).expect("parse channel result");

    let params_json = channel_result["params_json"]
        .as_str()
        .expect("params_json")
        .to_owned();
    let capacity_sat = channel_result["capacity"].as_u64().expect("capacity");
    let funding_amount_sat = channel_result["funding_token_amount"]
        .as_u64()
        .expect("funding_token_amount");

    // Derive channel_id
    let channel_id_hex =
        channel_parameters_get_channel_id(&params_json, &channel_secret_hex, &keyset_info_json)
            .expect("get channel_id");

    tracing::info!(
        "Channel: id={channel_id_hex} capacity={capacity_sat} funding={funding_amount_sat}"
    );

    // ─── Step 6: Create funding swap → blinded messages ───
    tracing::info!("Creating funding swap");
    let input_proofs_json = channel_result["proofs_json"]
        .as_str()
        .expect("proofs_json")
        .to_owned();

    let funding_swap_json = create_funding_swap(
        &params_json,
        &channel_secret_hex,
        &keyset_info_json,
        &input_proofs_json,
    )
    .expect("create_funding_swap");
    let funding_swap: serde_json::Value =
        serde_json::from_str(&funding_swap_json).expect("parse funding swap");

    let swap_request_json = funding_swap["swap_request_json"]
        .as_str()
        .expect("swap_request_json")
        .to_owned();
    let funding_secrets_json = funding_swap["funding_secrets_json"]
        .as_str()
        .expect("funding_secrets_json")
        .to_owned();

    // Extract blinded messages from the swap request
    let swap_request: serde_json::Value =
        serde_json::from_str(&swap_request_json).expect("parse swap request");
    let blinded_messages_array = swap_request["outputs"].as_array().expect("outputs array");

    let funding_blinded_messages: Vec<serde_json::Value> = blinded_messages_array
        .iter()
        .map(|bm| {
            serde_json::json!({
                "amount": bm["amount"],
                "B_": bm["B_"]
            })
        })
        .collect();

    tracing::info!(
        "Funding swap: {} blinded messages",
        funding_blinded_messages.len()
    );

    // ─── Step 7: POST swap to mint → blind signatures ───
    tracing::info!("Sending swap to mint");
    let swap_response_body = http_post(&format!("{MINT_URL}/v1/swap"), &swap_request_json)
        .await
        .expect("mint swap");

    let swap_response: serde_json::Value =
        serde_json::from_str(&swap_response_body).expect("parse swap response");
    let blind_signatures_array = swap_response["signatures"]
        .as_array()
        .expect("signatures array");

    let funding_blind_signatures: Vec<serde_json::Value> = blind_signatures_array
        .iter()
        .map(|sig| {
            // Include all fields needed by WASM construct_proofs:
            // amount, C_, id, and dleq (required for Spilman channels)
            let mut obj = serde_json::json!({
                "amount": sig["amount"],
                "C_": sig["C_"],
                "id": sig["id"],
            });
            if let Some(dleq) = sig.get("dleq") {
                obj["dleq"] = dleq.clone();
            }
            obj
        })
        .collect();

    tracing::info!(
        "Got {} blind signatures from mint",
        funding_blind_signatures.len()
    );

    // ─── Step 8: Complete funding swap → funding proofs ───
    tracing::info!("Completing funding swap");
    let complete_result_json = complete_funding_swap(
        &swap_response_body,
        &funding_secrets_json,
        &keyset_info_json,
    )
    .expect("complete_funding_swap");
    let complete_result: serde_json::Value =
        serde_json::from_str(&complete_result_json).expect("parse complete result");

    let funding_proofs_json = complete_result["funding_proofs_json"]
        .as_str()
        .expect("funding_proofs_json")
        .to_owned();

    let funding_proofs: Vec<serde_json::Value> =
        serde_json::from_str(&funding_proofs_json).expect("parse funding proofs");

    let constructed_proofs: Vec<serde_json::Value> = funding_proofs
        .iter()
        .map(|p| {
            serde_json::json!({
                "amount": p["amount"],
                "secret": p["secret"],
                "C": p["C"]
            })
        })
        .collect();

    tracing::info!("Constructed {} funding proofs", constructed_proofs.len());

    // ─── Step 9: Merge funding_secrets with blinded messages for full capture ───
    let funding_secrets: Vec<serde_json::Value> =
        serde_json::from_str(&funding_secrets_json).expect("parse funding secrets");

    let funding_blinded_messages_full: Vec<serde_json::Value> = blinded_messages_array
        .iter()
        .zip(funding_secrets.iter())
        .map(|(bm, sec)| {
            serde_json::json!({
                "amount": bm["amount"],
                "B_": bm["B_"],
                "blinding_factor_r": sec["blinding_factor"],
                "secret": sec["secret"]
            })
        })
        .collect();

    // ─── Step 10: Create signed balance update ───
    tracing::info!("Creating signed balance update (balance=30)");
    let amount_to_charlie: u64 = 30;

    // Get the unsigned balance update for message_hex and tweak info
    let unsigned_json = create_unsigned_balance_update(
        &params_json,
        &keyset_info_json,
        &channel_secret_hex,
        &funding_proofs_json,
        amount_to_charlie,
    )
    .expect("create_unsigned_balance_update");
    let unsigned: serde_json::Value =
        serde_json::from_str(&unsigned_json).expect("parse unsigned balance update");
    let message_hex = unsigned["message_hex"]
        .as_str()
        .expect("message_hex")
        .to_owned();
    let tweak_scalar_hex = unsigned["tweak_scalar_hex"]
        .as_str()
        .expect("tweak_scalar_hex")
        .to_owned();

    // Compute tweaked public key: sender_pubkey + tweak_scalar
    // We can derive it from the sender's key and the tweak.
    let tweaked_pub_hex = {
        let alice_pk: &secp256k1::PublicKey = &alice_pubkey;
        let (_, parity) = alice_pk.x_only_public_key();
        let base_sk: secp256k1::SecretKey = *alice_secret;
        let effective = if parity == Parity::Odd {
            base_sk.negate()
        } else {
            base_sk
        };
        let tweak_bytes = hex_decode(&tweak_scalar_hex).expect("tweak hex");
        let mut tweak_arr = [0u8; 32];
        tweak_arr.copy_from_slice(&tweak_bytes);
        let tweak = Scalar::from_be_bytes(tweak_arr).expect("tweak scalar");
        let adjusted_sk = effective.add_tweak(&tweak).expect("add tweak");
        let secp = Secp256k1::new();
        let derived_pk = secp256k1::PublicKey::from_secret_key(&secp, &adjusted_sk);
        derived_pk.to_string()
    };

    // Get the actual signature
    let signed_json = create_signed_balance_update(
        &params_json,
        &keyset_info_json,
        &alice_secret.to_secret_hex(),
        &funding_proofs_json,
        amount_to_charlie,
    )
    .expect("create_signed_balance_update");
    let signed: serde_json::Value =
        serde_json::from_str(&signed_json).expect("parse signed balance update");
    let signature_hex = signed["signature"].as_str().expect("signature").to_owned();

    tracing::info!("Balance update signed: amount={amount_to_charlie}");

    // ─── Step 11: Build JSON and write to file ───
    let test_vectors = serde_json::json!({
        "alice_seed_hex": ALICE_SEED_HEX,
        "alice_pubkey_hex": alice_pubkey_hex,
        "charlie_seed_hex": CHARLIE_SEED_HEX,
        "charlie_pubkey_hex": charlie_pubkey_hex,
        "ecdh_shared_secret_hex": ecdh_shared_secret_hex,
        "channel_secret_hex": channel_secret_hex,
        "params_json": params_json,
        "keyset_info_json": keyset_info_json,
        "funding_proofs_json": funding_proofs_json,
        "channel_id_hex": channel_id_hex,
        "keyset_id": keyset_id,
        "keyset_input_fee_ppk": keyset_input_fee_ppk,
        "funding_amount_sat": funding_amount_sat,
        "capacity_sat": capacity_sat,
        "maximum_amount_per_output": max_amount_per_output,
        "expiry_timestamp": expiry,
        "keyset_keys": keyset_keys_value,
        "funding_blinded_messages_sender_stage1": funding_blinded_messages_full,
        "funding_blind_signatures_sender_stage1": funding_blind_signatures,
        "constructed_proofs_sender_stage1": constructed_proofs,
        "signed_balance_update": {
            "amount_to_charlie": amount_to_charlie,
            "message_hex": message_hex,
            "signature_hex": signature_hex,
            "tweak_scalar_hex": tweak_scalar_hex,
            "tweaked_pub_hex": tweaked_pub_hex,
        }
    });

    let json_str = serde_json::to_string_pretty(&test_vectors).expect("serialize test vectors");

    let path = output_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    std::fs::write(&path, &json_str).expect("write test-vectors.json");

    tracing::info!("Test vectors written to {}", path.display());
    tracing::info!("JSON size: {} bytes", json_str.len());
    tracing::info!("=== Test Vector Capture Complete ===");
}

// Hex helpers (inlined to avoid importing cashu::util::hex).
#[cfg(feature = "spilman")]
fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err("hex string has odd length".to_owned());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| format!("hex decode at byte {}: {e}", i / 2))
        })
        .collect()
}
