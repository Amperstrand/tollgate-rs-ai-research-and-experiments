//! Spilman channel keyset utilities.
//!
//! Provides [`fetch_active_keyset_info`] for fetching the active sat keyset
//! from a Cashu mint, used during channel setup.
//!
//! The legacy [`SpilmanChannelManager`] struct is deprecated — channel operations
//! now use [`crate::spilman_service::SpilmanService`].

use std::time::Duration;

use cdk_spilman::{construct_proofs, parse_keyset_info_from_json, KeysetInfo};
use serde_json::Value;

/// Fetches the active sat keyset from the given mint URL.
///
/// Returns both the raw keyset JSON string and the parsed `KeysetInfo`.
///
/// # Errors
///
/// Returns an error if the mint is unreachable, returns malformed JSON,
/// or has no active sat keyset.
pub async fn fetch_active_keyset_info(mint_url: &str) -> Result<(String, KeysetInfo), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client: {e}"))?;

    let resp = client
        .get(format!("{mint_url}/v1/keysets"))
        .send()
        .await
        .map_err(|e| format!("GET /v1/keysets: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("read keysets body: {e}"))?;
    let body: Value = serde_json::from_str(&text).map_err(|e| format!("parse keysets: {e}"))?;

    let keysets = body["keysets"].as_array().ok_or("missing keysets array")?;

    let active_sat = keysets
        .iter()
        .find(|ks| ks["unit"].as_str() == Some("sat") && ks["active"].as_bool() == Some(true))
        .ok_or("no active sat keyset")?;

    let keyset_id = active_sat["id"].as_str().ok_or("missing keyset id")?;
    let input_fee_ppk = active_sat["input_fee_ppk"]
        .as_u64()
        .or_else(|| active_sat["inputFeePpk"].as_u64())
        .unwrap_or(0);

    let resp = client
        .get(format!("{mint_url}/v1/keys/{keyset_id}"))
        .send()
        .await
        .map_err(|e| format!("GET /v1/keys: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("read keys body: {e}"))?;
    let keys_body: Value = serde_json::from_str(&text).map_err(|e| format!("parse keys: {e}"))?;

    let keyset_data = keys_body["keysets"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or("missing keyset in keys response")?;

    let keyset_info_json = serde_json::json!({
        "keysetId": keyset_id,
        "unit": "sat",
        "keys": keyset_data["keys"],
        "inputFeePpk": input_fee_ppk
    })
    .to_string();

    let keyset_info = parse_keyset_info_from_json(&keyset_info_json)?;
    Ok((keyset_info_json, keyset_info))
}

#[deprecated(
    since = "0.2.0",
    note = "Use fetch_active_keyset_info() or SpilmanService for channel ops."
)]
pub struct SpilmanChannelManager {
    mint_url: String,
    client: reqwest::Client,
}

#[allow(deprecated)]
impl SpilmanChannelManager {
    /// Creates a new Spilman channel manager targeting the given mint URL.
    #[allow(clippy::missing_panics_doc)]
    pub fn new(mint_url: &str) -> Self {
        Self {
            mint_url: mint_url.to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn mint_url(&self) -> &str {
        &self.mint_url
    }

    #[allow(clippy::missing_errors_doc)]
    /// Fetches the active sat keyset from the mint.
    ///
    /// Delegates to [`fetch_active_keyset_info`].
    pub async fn fetch_active_keyset_info(&self) -> Result<(String, KeysetInfo), String> {
        fetch_active_keyset_info(&self.mint_url).await
    }

    /// Mints funding proofs from the given deterministic blinded outputs.
    ///
    /// This creates a bolt11 quote, polls until paid (testnut auto-pays),
    /// then mints blind signatures and constructs spendable proofs.
    ///
    /// # Errors
    ///
    /// Returns an error if any HTTP request fails, the quote is not paid within
    /// 60 seconds, the mint response is malformed, or proof construction fails.
    pub async fn mint_proofs_from_funding_outputs(
        &self,
        funding_outputs_json: &str,
        keyset_info_json: &str,
    ) -> Result<String, String> {
        let outputs: Value = serde_json::from_str(funding_outputs_json)
            .map_err(|e| format!("parse funding outputs: {e}"))?;

        let funding_nominal = outputs["funding_token_nominal"]
            .as_u64()
            .ok_or("missing funding_token_nominal")?;
        let blinded_messages = &outputs["blinded_messages"];
        let secrets_with_blinding = outputs["secrets_with_blinding"].to_string();

        let quote_body = serde_json::json!({
            "amount": funding_nominal,
            "unit": "sat"
        })
        .to_string();

        let resp = self
            .client
            .post(format!("{}/v1/mint/quote/bolt11", self.mint_url))
            .header("Content-Type", "application/json")
            .body(quote_body)
            .send()
            .await
            .map_err(|e| format!("POST /v1/mint/quote/bolt11: {e}"))?;
        let text = resp
            .text()
            .await
            .map_err(|e| format!("read quote body: {e}"))?;
        let quote: Value = serde_json::from_str(&text).map_err(|e| format!("parse quote: {e}"))?;
        let quote_id = quote["quote"].as_str().ok_or("missing quote id")?;

        tracing::info!("[Spilman] Mint quote {quote_id} created for {funding_nominal} sat");

        for i in 0..120u32 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let resp = self
                .client
                .get(format!("{}/v1/mint/quote/bolt11/{quote_id}", self.mint_url))
                .send()
                .await
                .map_err(|e| format!("poll quote: {e}"))?;
            let text = resp
                .text()
                .await
                .map_err(|e| format!("read poll body: {e}"))?;
            let status: Value =
                serde_json::from_str(&text).map_err(|e| format!("parse poll: {e}"))?;

            if status["state"].as_str() == Some("PAID") {
                tracing::info!("[Spilman] Quote {quote_id} PAID after {} polls", i + 1);
                break;
            }
            if i == 119 {
                return Err(format!("quote {quote_id} not paid after 60s"));
            }
        }

        let mint_body = serde_json::json!({
            "quote": quote_id,
            "outputs": blinded_messages
        })
        .to_string();

        let resp = self
            .client
            .post(format!("{}/v1/mint/bolt11", self.mint_url))
            .header("Content-Type", "application/json")
            .body(mint_body)
            .send()
            .await
            .map_err(|e| format!("POST /v1/mint/bolt11: {e}"))?;
        let text = resp
            .text()
            .await
            .map_err(|e| format!("read mint body: {e}"))?;
        let mint_result: Value =
            serde_json::from_str(&text).map_err(|e| format!("parse mint: {e}"))?;

        let signatures = mint_result["signatures"]
            .as_array()
            .ok_or("missing signatures in mint response")?;
        let signatures_json =
            serde_json::to_string(signatures).map_err(|e| format!("serialize signatures: {e}"))?;

        tracing::info!(
            "[Spilman] Got {} blind signatures from mint",
            signatures.len()
        );

        construct_proofs(&signatures_json, &secrets_with_blinding, keyset_info_json)
    }
}
