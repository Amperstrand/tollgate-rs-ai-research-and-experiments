//! cdk-spilman Bridge Spike Test
//!
//! Proves that cdk-spilman's bridge infrastructure (`SpilmanClientBridge`,
//! `SpilmanBridge`) works end-to-end against a real Cashu mint (testnut).
//!
//! This replaces our hand-rolled `SpilmanChannelManager` with cdk-spilman's
//! built-in bridge API. If this spike compiles and passes, we'll wire it
//! into `tollgate-net` for production use.
//!
//! Run with:
//!   cargo test -p tollgate-net --test cdk_spilman_bridge_spike \
//!     --features spilman -- --ignored --nocapture

mod common;

#[cfg(feature = "spilman")]
use {
    cashu::nuts::{CurrencyUnit, Id, PublicKey, SecretKey},
    cdk_spilman::{
        compute_channel_secret_from_hex, sign_with_tweaked_key_util, ChannelFunding,
        ChannelPolicy, ChannelState, ClosingData, ConfigurableClientHost, MemoryClientStorage,
        PaymentProof, SpilmanBridge, SpilmanClientAsyncNetworking, SpilmanClientBridge,
        SpilmanClientNetworking, SpilmanHost,
    },
    std::cell::{Cell, RefCell},
    std::collections::HashMap,
    std::time::{SystemTime, UNIX_EPOCH},
    tollgate_core::types::Amount,
    tollgate_core::wallet::Wallet,
    tollgate_net::cdk_wallet::CdkWallet,
    tollgate_net::spilman_wallet::SpilmanChannelManager,
};

#[cfg(feature = "spilman")]
const MINT_URL: &str = "https://testnut.cashu.exchange";

// ---------------------------------------------------------------------------
// Networking implementations
// ---------------------------------------------------------------------------

/// Async networking that calls the mint's `/v1/swap` via reqwest.
#[cfg(feature = "spilman")]
struct TestnutAsyncNetworking {
    client: reqwest::Client,
}

#[cfg(feature = "spilman")]
impl TestnutAsyncNetworking {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "spilman")]
#[async_trait::async_trait]
impl SpilmanClientAsyncNetworking for TestnutAsyncNetworking {
    async fn call_mint_swap(
        &self,
        mint_url: &str,
        swap_request_json: &str,
    ) -> Result<String, String> {
        let url = format!("{mint_url}/v1/swap");
        tracing::info!("[spike] POST {url} ({} bytes)", swap_request_json.len());

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

/// Dummy sync networking — only needed for `SpilmanClientBridge` construction.
/// All actual mint communication uses the async path.
#[cfg(feature = "spilman")]
struct DummySyncNetworking;

#[cfg(feature = "spilman")]
impl SpilmanClientNetworking for DummySyncNetworking {
    fn call_mint_swap(&self, _mint_url: &str, _json: &str) -> Result<String, String> {
        panic!("sync networking not used — use async path instead")
    }
}

// ---------------------------------------------------------------------------
// Server host — minimal SpilmanHost<()> for the spike
// ---------------------------------------------------------------------------

/// In-memory `SpilmanHost<()>` modeled after cdk-spilman's `TestServerHost`.
/// Accepts any mint/keyset, stores channels and payments in `RefCell` maps.
#[cfg(feature = "spilman")]
struct SpikeServerHost {
    receiver_secret: SecretKey,
    channels: RefCell<HashMap<String, ChannelFunding>>,
    payments: RefCell<HashMap<String, PaymentProof>>,
    states: RefCell<HashMap<String, ChannelState>>,
    closing_data: RefCell<HashMap<String, ClosingData>>,
    keyset_infos: RefCell<HashMap<Id, String>>,
    active_keysets: RefCell<HashMap<String, Vec<Id>>>,
    amount_due: Cell<u64>,
}

#[cfg(feature = "spilman")]
impl SpikeServerHost {
    fn new(receiver_secret: SecretKey) -> Self {
        Self {
            receiver_secret,
            channels: RefCell::new(HashMap::new()),
            payments: RefCell::new(HashMap::new()),
            states: RefCell::new(HashMap::new()),
            closing_data: RefCell::new(HashMap::new()),
            keyset_infos: RefCell::new(HashMap::new()),
            active_keysets: RefCell::new(HashMap::new()),
            amount_due: Cell::new(0),
        }
    }

    fn add_keyset(&self, mint_url: &str, keyset_id: Id, keyset_info_json: String) {
        self.keyset_infos
            .borrow_mut()
            .insert(keyset_id, keyset_info_json);
        self.active_keysets
            .borrow_mut()
            .entry(mint_url.to_string())
            .or_default()
            .push(keyset_id);
    }

    fn get_last_payment(&self, channel_id: &str) -> Option<PaymentProof> {
        self.payments.borrow().get(channel_id).cloned()
    }
}

#[cfg(feature = "spilman")]
impl SpilmanHost<()> for SpikeServerHost {
    fn receiver_key_is_acceptable(&self, pk: &PublicKey) -> bool {
        *pk == self.receiver_secret.public_key()
    }

    fn mint_and_keyset_is_acceptable(&self, _mint: &str, _id: &Id) -> bool {
        true
    }

    fn get_funding(&self, cid: &str) -> Option<ChannelFunding> {
        self.channels.borrow().get(cid).cloned()
    }

    fn save_funding(&self, cid: &str, funding: ChannelFunding, payment: PaymentProof) {
        self.channels
            .borrow_mut()
            .insert(cid.to_string(), funding);
        self.payments
            .borrow_mut()
            .insert(cid.to_string(), payment);
        self.states
            .borrow_mut()
            .insert(cid.to_string(), ChannelState::Open);
    }

    fn get_amount_due(&self, _cid: &str, _ctx: Option<&()>) -> u64 {
        self.amount_due.get()
    }

    fn record_payment(&self, cid: &str, payment: PaymentProof, _ctx: &()) {
        self.payments
            .borrow_mut()
            .insert(cid.to_string(), payment);
    }

    fn get_channel_state(&self, cid: &str) -> ChannelState {
        self.states
            .borrow()
            .get(cid)
            .copied()
            .unwrap_or(ChannelState::Open)
    }

    fn mark_channel_closing(
        &self,
        cid: &str,
        expiry_ts: u64,
        payment: PaymentProof,
    ) -> Result<(), String> {
        self.states
            .borrow_mut()
            .insert(cid.to_string(), ChannelState::Closing);
        self.closing_data.borrow_mut().insert(
            cid.to_string(),
            ClosingData {
                expiry_timestamp: expiry_ts,
                balance: payment.balance,
                signature: payment.signature,
            },
        );
        Ok(())
    }

    fn get_closing_data(&self, cid: &str) -> Option<ClosingData> {
        self.closing_data.borrow().get(cid).cloned()
    }

    fn get_channel_policy(&self, _unit: &str) -> Option<ChannelPolicy> {
        Some(ChannelPolicy {
            min_capacity: 1,
            min_expiry_in_seconds: 60,
            max_amount_per_output: Some(64),
        })
    }

    fn now_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn get_balance_and_signature_for_unilateral_exit(
        &self,
        cid: &str,
    ) -> Option<PaymentProof> {
        self.payments.borrow().get(cid).cloned()
    }

    fn get_active_keyset_ids(&self, mint: &str, _unit: &CurrencyUnit) -> Vec<Id> {
        self.active_keysets
            .borrow()
            .get(mint)
            .cloned()
            .unwrap_or_default()
    }

    fn get_keyset_info(&self, _mint: &str, kid: &Id) -> Option<String> {
        self.keyset_infos.borrow().get(kid).cloned()
    }

    fn mark_channel_closed(
        &self,
        cid: &str,
        _expiry_ts: u64,
        _balance: u64,
        _receiver_proofs: &str,
        _sender_proofs: &str,
        _receiver_sum: u64,
        _sender_sum: u64,
    ) -> Result<(), String> {
        self.states
            .borrow_mut()
            .insert(cid.to_string(), ChannelState::Closed);
        Ok(())
    }

    fn compute_channel_secret(
        &self,
        receiver_pk_hex: &str,
        sender_pk_hex: &str,
    ) -> Result<String, String> {
        let expected = self.receiver_secret.public_key().to_hex();
        if receiver_pk_hex != expected {
            return Err(format!(
                "receiver pubkey mismatch: expected {expected}, got {receiver_pk_hex}"
            ));
        }
        compute_channel_secret_from_hex(&self.receiver_secret.to_secret_hex(), sender_pk_hex)
    }

    fn sign_with_tweaked_key(
        &self,
        signer_pk_hex: &str,
        message_hex: &str,
        tweak_hex: &str,
    ) -> Result<String, String> {
        let expected = self.receiver_secret.public_key().to_hex();
        if signer_pk_hex != expected {
            return Err(format!(
                "signer pubkey mismatch: expected {expected}, got {signer_pk_hex}"
            ));
        }
        sign_with_tweaked_key_util(&self.receiver_secret.to_secret_hex(), message_hex, tweak_hex)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// The spike test
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
#[allow(clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires network access to testnut.cashu.exchange"]
async fn cdk_spilman_bridge_spike() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=debug,info")
        .with_test_writer()
        .try_init();

    tracing::info!("=== cdk-spilman Bridge Spike Test ===");
    tracing::info!("Mint: {MINT_URL}");

    // ─── Phase 1: Mint proofs from testnut via CdkWallet ───
    tracing::info!("Phase 1: Minting tokens from testnut via CdkWallet");
    let wallet = CdkWallet::new(MINT_URL, [42u8; 64])
        .await
        .expect("CdkWallet init");
    wallet
        .mint_test_tokens(2000)
        .await
        .expect("mint 2000 sat from testnut");
    let bal = wallet.total_balance().await.expect("balance check");
    tracing::info!("Wallet balance after mint: {bal} sat");
    assert!(bal >= 1000, "need >= 1000 sat for channel, got {bal}");

    // Create a token for 1000 sat — plenty for a channel
    let token_bytes = wallet
        .create_token(Amount(1000), MINT_URL)
        .await
        .expect("create 1000 sat token");
    let token_str = String::from_utf8(token_bytes).expect("token is valid UTF-8");
    tracing::info!("Token created: {} bytes", token_str.len());

    // ─── Phase 2: Fetch keyset info from testnut ───
    tracing::info!("Phase 2: Fetching active keyset from testnut");
    let mgr = SpilmanChannelManager::new(MINT_URL);
    let (keyset_info_json, keyset_info) = mgr
        .fetch_active_keyset_info()
        .await
        .expect("fetch keyset info");
    let keyset_id = keyset_info.keyset_id;
    tracing::info!("Keyset: id={keyset_id} fee_ppk={}", keyset_info.input_fee_ppk);

    // ─── Phase 3: Setup client bridge (buyer) and server bridge (seller) ───
    tracing::info!("Phase 3: Setting up bridges");

    // Buyer side — SpilmanClientBridge
    let sender_secret = SecretKey::generate();
    let sender_pubkey_hex = sender_secret.public_key().to_hex();
    let mut client_host = ConfigurableClientHost::new(MemoryClientStorage::new());
    client_host.add_key(sender_secret.clone());
    let client_bridge = SpilmanClientBridge::new(client_host, DummySyncNetworking);
    let async_net = TestnutAsyncNetworking::new();

    // Seller side — SpilmanBridge
    let receiver_secret = SecretKey::generate();
    let receiver_pubkey_hex = receiver_secret.public_key().to_hex();
    let server_host = SpikeServerHost::new(receiver_secret);
    server_host.add_keyset(MINT_URL, keyset_id, keyset_info_json.clone());
    let server_bridge = SpilmanBridge::new(server_host);

    tracing::info!(
        "Buyer pubkey: {}...",
        &sender_pubkey_hex[..sender_pubkey_hex.len().min(16)]
    );
    tracing::info!(
        "Seller pubkey: {}...",
        &receiver_pubkey_hex[..receiver_pubkey_hex.len().min(16)]
    );

    // ─── Phase 4: Open channel from token ───
    tracing::info!("Phase 4: Opening Spilman channel from Cashu token");
    let expiry = now_secs() + 3600;
    let open_result = client_bridge
        .open_channel_from_token_async(
            &token_str,
            &receiver_pubkey_hex,
            &sender_pubkey_hex,
            expiry,
            &keyset_info_json,
            64,
            &async_net,
        )
        .await
        .expect("open channel from token");

    tracing::info!(
        "Channel opened: id={} capacity={} sat funding_amount={} sat",
        open_result.channel_id,
        open_result.capacity,
        open_result.funding_token_amount,
    );
    assert!(!open_result.channel_id.is_empty(), "channel ID must not be empty");
    assert!(open_result.capacity > 0, "capacity must be positive");

    // ─── Phase 5: First payment with funding ───
    tracing::info!("Phase 5: Creating first payment (balance=10 sat, with funding)");
    let payment1 = client_bridge
        .create_payment_with_funding(&open_result.channel_id, 10)
        .expect("create payment 1");
    assert_eq!(payment1.balance, 10);
    assert!(payment1.has_funding(), "first payment must include funding");
    tracing::info!(
        "Payment 1: channel={} balance={} has_funding={}",
        &payment1.channel_id[..payment1.channel_id.len().min(16)],
        payment1.balance,
        payment1.has_funding(),
    );

    // Server processes first payment (registers channel)
    let result1 = server_bridge
        .process_payment(
            &payment1.channel_id,
            payment1.balance,
            &payment1.signature,
            payment1.params.as_ref(),
            payment1.funding_proofs.as_deref(),
            &(),
        )
        .expect("server accepts payment 1");
    tracing::info!("Server accepted payment 1: balance={}", result1.balance);
    assert_eq!(result1.balance, 10);

    // ─── Phase 6: Second payment (no funding needed) ───
    tracing::info!("Phase 6: Creating second payment (balance=25 sat, no funding)");
    let payment2 = client_bridge
        .create_payment(&open_result.channel_id, 25)
        .expect("create payment 2");
    assert_eq!(payment2.balance, 25);
    assert!(!payment2.has_funding(), "subsequent payment has no funding");

    let result2 = server_bridge
        .process_payment(
            &payment2.channel_id,
            payment2.balance,
            &payment2.signature,
            None,
            None,
            &(),
        )
        .expect("server accepts payment 2");
    tracing::info!("Server accepted payment 2: balance={}", result2.balance);
    assert_eq!(result2.balance, 25);

    // ─── Phase 7: Third payment ───
    tracing::info!("Phase 7: Creating third payment (balance=50 sat)");
    let payment3 = client_bridge
        .create_payment(&open_result.channel_id, 50)
        .expect("create payment 3");
    assert_eq!(payment3.balance, 50);

    let result3 = server_bridge
        .process_payment(
            &payment3.channel_id,
            payment3.balance,
            &payment3.signature,
            None,
            None,
            &(),
        )
        .expect("server accepts payment 3");
    assert_eq!(result3.balance, 50);

    // ─── Phase 8: Verify final state ───
    tracing::info!("Phase 8: Verifying final channel state");

    // Client side
    let client_info = client_bridge
        .get_channel_info(&open_result.channel_id)
        .expect("client channel info");
    assert_eq!(client_info.current_balance, 50);
    assert_eq!(client_info.payment_count, 3);
    tracing::info!(
        "Client state: balance={} payments={} capacity={}",
        client_info.current_balance,
        client_info.payment_count,
        client_info.capacity,
    );

    // Server side
    let server_payment = server_bridge
        .host()
        .get_last_payment(&open_result.channel_id)
        .expect("server has last payment");
    assert_eq!(server_payment.balance, 50);
    tracing::info!("Server state: balance={}", server_payment.balance);

    // ─── Summary ───
    tracing::info!("");
    tracing::info!("=== cdk-spilman Bridge Spike Complete ===");
    tracing::info!("Channel ID: {}", open_result.channel_id);
    tracing::info!("Capacity: {} sat", open_result.capacity);
    tracing::info!("Payments: 3 (10, 25, 50 sat)");
    tracing::info!(
        "Final state: 50 sat to seller, {} sat refund to buyer",
        open_result.capacity.saturating_sub(50)
    );
    tracing::info!("SPIKE SUCCESS: cdk-spilman bridge works against testnut!");
}
