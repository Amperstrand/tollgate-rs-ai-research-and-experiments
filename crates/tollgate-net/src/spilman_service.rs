use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cashu::nuts::SecretKey;
use cdk_spilman::{
    ConfigurableClientHost, MemoryClientStorage, SpilmanClientAsyncNetworking,
    SpilmanClientBridge, SpilmanClientNetworking,
};

pub use cdk_spilman::{
    ClientChannelInfo, CloseSuccess, OpenChannelResult, Payment, PaymentProof, PaymentSuccess,
    SpilmanAsyncNetworking, SpilmanBridge, SpilmanHost,
};

pub struct ReqwestNetworking {
    client: reqwest::Client,
}

impl Default for ReqwestNetworking {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestNetworking {
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
}

pub struct DummySyncNetworking;

impl SpilmanClientNetworking for DummySyncNetworking {
    fn call_mint_swap(&self, _mint_url: &str, _json: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }
}

type ClientBridge =
    SpilmanClientBridge<ConfigurableClientHost<MemoryClientStorage>, DummySyncNetworking>;

pub struct SpilmanService {
    client_bridge: ClientBridge,
    mint_url: String,
    sender_pubkey_hex: String,
}

impl SpilmanService {
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

    pub fn mint_url(&self) -> &str {
        &self.mint_url
    }

    pub fn sender_pubkey(&self) -> &str {
        &self.sender_pubkey_hex
    }

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

    #[allow(clippy::missing_errors_doc)]
    pub fn create_payment(&self, channel_id: &str, balance: u64) -> Result<Payment, String> {
        self.client_bridge.create_payment(channel_id, balance)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn create_payment_with_funding(
        &self,
        channel_id: &str,
        balance: u64,
    ) -> Result<Payment, String> {
        self.client_bridge
            .create_payment_with_funding(channel_id, balance)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn request_cooperative_close(
        &self,
        channel_id: &str,
        final_balance: u64,
    ) -> Result<Payment, String> {
        self.client_bridge
            .create_cooperative_close_request(channel_id, final_balance)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn confirm_cooperative_close(&self, response_json: &str) -> Result<(), String> {
        self.client_bridge
            .process_cooperative_close_response(response_json)
    }

    pub fn get_channel_info(&self, channel_id: &str) -> Option<ClientChannelInfo> {
        self.client_bridge.get_channel_info(channel_id)
    }
}
