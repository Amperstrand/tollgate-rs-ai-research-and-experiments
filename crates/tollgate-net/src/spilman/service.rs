//! Spilman channel client service.
//!
//! Provides [`SpilmanService`] — a thin wrapper around
//! [`cdk_spilman::SpilmanClientBridge`] that manages channel open/payment/close
//! operations against a Cashu mint. All cross-version communication uses JSON
//! strings so the cdk-spilman version boundary stays isolated.

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cashu::nuts::SecretKey;
use cdk_spilman::{
    ConfigurableClientHost, MemoryClientStorage, SpilmanClientAsyncNetworking,
    SpilmanClientBridge, SpilmanClientNetworking,
};

#[allow(unused_imports)]
pub use cdk_spilman::{
    ClientChannelInfo, CloseSuccess, OpenChannelResult, Payment, PaymentProof, PaymentSuccess,
    SpilmanAsyncNetworking, SpilmanBridge, SpilmanHost,
};

/// Reqwest-based async networking implementation for Spilman client calls.
pub struct ReqwestNetworking {
    client: reqwest::Client,
}

impl Default for ReqwestNetworking {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestNetworking {
    /// Creates a new networking instance with a default reqwest client.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl SpilmanClientAsyncNetworking for ReqwestNetworking {
    async fn call_mint_swap(
        &self,
        mint_url: &str,
        swap_request_json: &str,
    ) -> Result<String, String> {
        let url = format!("{mint_url}/v1/swap");
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(swap_request_json.to_string())
            .send()
            .await
            .map_err(|e| format!("swap request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("swap failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read swap response: {e}"))
    }

    async fn call_mint_keysets(&self, mint_url: &str) -> Result<String, String> {
        let url = format!("{mint_url}/v1/keysets");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("keysets request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("keysets failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read keysets response: {e}"))
    }

    async fn call_mint_keys(&self, mint_url: &str, keyset_id: &str) -> Result<String, String> {
        let url = format!("{mint_url}/v1/keys/{keyset_id}");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("keys request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("keys failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read keys response: {e}"))
    }

    async fn call_mint_restore(
        &self,
        mint_url: &str,
        restore_request_json: &str,
    ) -> Result<String, String> {
        let url = format!("{mint_url}/v1/restore");
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(restore_request_json.to_string())
            .send()
            .await
            .map_err(|e| format!("restore request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("restore failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read restore response: {e}"))
    }
}

/// Placeholder sync networking — the async path is always used.
pub struct DummySyncNetworking;

impl SpilmanClientNetworking for DummySyncNetworking {
    fn call_mint_swap(&self, _mint_url: &str, _json: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }

    fn call_mint_keysets(&self, _mint_url: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }

    fn call_mint_keys(&self, _mint_url: &str, _keyset_id: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }

    fn call_mint_restore(&self, _mint_url: &str, _json: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }
}

type ClientBridge =
    SpilmanClientBridge<ConfigurableClientHost<MemoryClientStorage>, DummySyncNetworking>;

/// Spilman channel client service.
///
/// Wraps a [`SpilmanClientBridge`] configured with in-memory storage and the
/// sender's secret key. All mint communication flows through JSON, keeping the
/// cdk-spilman version boundary isolated from the rest of the node.
pub struct SpilmanService {
    client_bridge: ClientBridge,
    mint_url: String,
    sender_pubkey_hex: String,
}

impl SpilmanService {
    /// Creates a new service targeting `mint_url`, authenticated as `sender_secret`.
    pub fn new(mint_url: &str, sender_secret: SecretKey) -> Self {
        let sender_pubkey_hex = sender_secret.public_key().to_hex();
        let mut host = ConfigurableClientHost::new(MemoryClientStorage::new());
        host.add_key(sender_secret);
        let client_bridge = SpilmanClientBridge::new(host, DummySyncNetworking);
        Self {
            client_bridge,
            mint_url: mint_url.to_owned(),
            sender_pubkey_hex,
        }
    }

    /// Returns the mint URL this service targets.
    pub fn mint_url(&self) -> &str {
        &self.mint_url
    }

    /// Returns the sender's public key in hex.
    pub fn sender_pubkey(&self) -> &str {
        &self.sender_pubkey_hex
    }

    /// Opens a Spilman channel funded by `token_str` to `receiver_pubkey_hex`.
    ///
    /// # Errors
    ///
    /// Returns a `String` error if the cdk-spilman bridge fails to open the
    /// channel or if the mint returns an error.
    #[allow(clippy::missing_errors_doc)]
    pub async fn open_channel(
        &self,
        token_str: &str,
        receiver_pubkey_hex: &str,
        expiry_secs: u64,
        keyset_info_json: &str,
        max_amount_per_output: u64,
        net: &ReqwestNetworking,
    ) -> Result<OpenChannelResult, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let expiry_timestamp = now.saturating_add(expiry_secs);

        self.client_bridge
            .open_channel_from_token_async(
                token_str,
                receiver_pubkey_hex,
                &self.sender_pubkey_hex,
                expiry_timestamp,
                keyset_info_json,
                max_amount_per_output,
                net,
            )
            .await
    }

    /// Creates a payment of `balance` sats on the given channel.
    ///
    /// # Errors
    ///
    /// Returns a `String` error if the bridge cannot create the payment.
    #[allow(clippy::missing_errors_doc)]
    pub fn create_payment(&self, channel_id: &str, balance: u64) -> Result<Payment, String> {
        self.client_bridge.create_payment(channel_id, balance)
    }

    /// Creates a payment that includes additional funding proofs.
    ///
    /// # Errors
    ///
    /// Returns a `String` error if the bridge cannot create the payment.
    #[allow(clippy::missing_errors_doc)]
    pub fn create_payment_with_funding(
        &self,
        channel_id: &str,
        balance: u64,
    ) -> Result<Payment, String> {
        self.client_bridge
            .create_payment_with_funding(channel_id, balance)
    }

    /// Creates a cooperative close request for the channel's final balance.
    ///
    /// # Errors
    ///
    /// Returns a `String` error if the bridge cannot create the request.
    #[allow(clippy::missing_errors_doc)]
    pub fn request_cooperative_close(
        &self,
        channel_id: &str,
        final_balance: u64,
    ) -> Result<Payment, String> {
        self.client_bridge
            .create_cooperative_close_request(channel_id, final_balance)
    }

    /// Processes a cooperative close response from the counterparty.
    ///
    /// # Errors
    ///
    /// Returns a `String` error if the response is invalid.
    #[allow(clippy::missing_errors_doc)]
    pub fn confirm_cooperative_close(&self, response_json: &str) -> Result<(), String> {
        self.client_bridge
            .process_cooperative_close_response(response_json)
    }

    /// Returns channel info if the channel exists in local storage.
    pub fn get_channel_info(&self, channel_id: &str) -> Option<ClientChannelInfo> {
        self.client_bridge.get_channel_info(channel_id)
    }
}
