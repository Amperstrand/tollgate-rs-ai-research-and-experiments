//! SpilmanService Integration Test
//!
//! Tests the production `SpilmanService` wrapper against a real Cashu mint (testnut).
//! Uses `SpilmanService` for the buyer side and `SpilmanBridge` + `SpikeServerHost`
//! for the seller side.
//!
//! Run with:
//!   cargo test -p tollgate-net --test spilman_service_integration \
//!     --features spilman -- --ignored --nocapture

mod common;

#[cfg(feature = "spilman")]
use {
    cashu::mint_url::MintUrl,
    cashu::nuts::Token as CashuToken,
    cashu::nuts::{CurrencyUnit, Id, Proof as CashuProof, PublicKey, SecretKey},
    cdk_spilman::{
        compute_channel_secret_from_hex, sign_with_tweaked_key_util, ChannelFunding, ChannelPolicy,
        ChannelState, ClosingData, PaymentProof, SpilmanAsyncNetworking, SpilmanBridge,
        SpilmanHost,
    },
    std::cell::{Cell, RefCell},
    std::collections::HashMap,
    std::str::FromStr,
    std::time::{SystemTime, UNIX_EPOCH},
    tollgate_net::cdk_wallet::CdkWallet,
    tollgate_net::spilman_service::{ReqwestNetworking, SpilmanService},
    tollgate_net::spilman_wallet::SpilmanChannelManager,
};

#[cfg(feature = "spilman")]
const MINT_URL: &str = "https://testnut.cashu.exchange";

// ---------------------------------------------------------------------------
// Server-side async networking (swaps proofs at mint during cooperative close)
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
struct ServerAsyncNetworking {
    client: reqwest::Client,
}

#[cfg(feature = "spilman")]
impl ServerAsyncNetworking {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "spilman")]
#[async_trait::async_trait]
impl SpilmanAsyncNetworking for ServerAsyncNetworking {
    async fn call_mint_swap(
        &self,
        mint_url: &str,
        swap_request_json: &str,
    ) -> Result<String, String> {
        let url = format!("{mint_url}/v1/swap");
        tracing::info!(
            "[server-net] POST {url} ({} bytes)",
            swap_request_json.len()
        );

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(swap_request_json.to_string())
            .send()
            .await
            .map_err(|e| format!("server swap failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("server swap failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read server swap response: {e}"))
    }

    async fn refresh_all_keysets(&self, mint: &str) -> Result<(), String> {
        tracing::info!("[server-net] keyset refresh skipped (mint={mint})");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Server host — minimal SpilmanHost<()> for the test
// ---------------------------------------------------------------------------

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
        self.channels.borrow_mut().insert(cid.to_string(), funding);
        self.payments.borrow_mut().insert(cid.to_string(), payment);
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
            .insert(cid.to_string(), payment.clone());
        self.amount_due.set(payment.balance);
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

    fn get_balance_and_signature_for_unilateral_exit(&self, cid: &str) -> Option<PaymentProof> {
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
        sign_with_tweaked_key_util(
            &self.receiver_secret.to_secret_hex(),
            message_hex,
            tweak_hex,
        )
    }
}

// ---------------------------------------------------------------------------
// The integration test
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
#[tokio::test]
#[ignore = "requires network access to testnut.cashu.exchange"]
async fn spilman_service_full_lifecycle() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=debug,info")
        .with_test_writer()
        .try_init();

    tracing::info!("=== SpilmanService Integration Test ===");
    tracing::info!("Mint: {MINT_URL}");

    // ─── Phase 1: Mint proofs from testnut via CdkWallet ───
    tracing::info!("Phase 1: Minting tokens from testnut via CdkWallet");
    let wallet = CdkWallet::new(MINT_URL, rand::random())
        .await
        .expect("CdkWallet init");
    wallet
        .mint_test_tokens(2000)
        .await
        .expect("mint 2000 sat from testnut");
    let bal = wallet.total_balance().await.expect("balance check");
    tracing::info!("Wallet balance after mint: {bal} sat");
    assert!(bal >= 1000, "need >= 1000 sat for channel, got {bal}");

    // Extract raw proofs (bypass CDK's prepare_send+confirm which causes double-spend
    // when cdk-spilman tries to swap again). Build token with cashu v0.15.1 types.
    let proofs_json = wallet.unspent_proofs_json().await.expect("get proofs");
    let all_proofs: Vec<CashuProof> =
        serde_json::from_str(&proofs_json).expect("parse cashu v0.15.1 proofs");
    tracing::info!("Extracted {} unspent proofs from wallet", all_proofs.len());

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
    tracing::info!("Token created: {} bytes", token_str.len());

    // ─── Phase 2: Fetch keyset info from testnut ───
    tracing::info!("Phase 2: Fetching active keyset from testnut");
    let mgr = SpilmanChannelManager::new(MINT_URL);
    let (keyset_info_json, keyset_info) = mgr
        .fetch_active_keyset_info()
        .await
        .expect("fetch keyset info");
    let keyset_id = keyset_info.keyset_id;
    tracing::info!(
        "Keyset: id={keyset_id} fee_ppk={}",
        keyset_info.input_fee_ppk
    );

    // ─── Phase 3: Create buyer (SpilmanService) and seller (SpilmanBridge) ───
    tracing::info!("Phase 3: Setting up SpilmanService (buyer) and SpilmanBridge (seller)");

    let sender_secret = SecretKey::generate();
    let spilman_service = SpilmanService::new(MINT_URL, sender_secret);
    let sender_pubkey_hex = spilman_service.sender_pubkey().to_owned();
    let net = ReqwestNetworking::new();

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

    // ─── Phase 4: Open channel via SpilmanService ───
    tracing::info!("Phase 4: Opening Spilman channel via SpilmanService");
    let open_result = spilman_service
        .open_channel(
            &token_str,
            &receiver_pubkey_hex,
            3600,
            &keyset_info_json,
            64,
            &net,
        )
        .await
        .expect("open channel via SpilmanService");

    let channel_id = open_result.channel_id.clone();
    tracing::info!(
        "Channel opened: id={} capacity={} sat funding_amount={} sat",
        channel_id,
        open_result.capacity,
        open_result.funding_token_amount,
    );
    assert!(!channel_id.is_empty(), "channel ID must not be empty");
    assert!(open_result.capacity > 0, "capacity must be positive");

    // ─── Phase 5: Payment 1 — with funding (10 sat) ───
    tracing::info!("Phase 5: Payment 1 (balance=10 sat, with funding)");
    let payment1 = spilman_service
        .create_payment_with_funding(&channel_id, 10)
        .expect("create payment 1");
    assert_eq!(payment1.balance, 10);
    assert!(payment1.has_funding(), "first payment must include funding");
    tracing::info!(
        "Payment 1: channel={} balance={} has_funding={}",
        &channel_id[..channel_id.len().min(16)],
        payment1.balance,
        payment1.has_funding(),
    );

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
    assert_eq!(result1.balance, 10);
    tracing::info!("Server accepted payment 1: balance={}", result1.balance);

    // ─── Phase 6: Payment 2 (25 sat, no funding) ───
    tracing::info!("Phase 6: Payment 2 (balance=25 sat)");
    let payment2 = spilman_service
        .create_payment(&channel_id, 25)
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
    assert_eq!(result2.balance, 25);
    tracing::info!("Server accepted payment 2: balance={}", result2.balance);

    // ─── Phase 7: Payment 3 (50 sat) ───
    tracing::info!("Phase 7: Payment 3 (balance=50 sat)");
    let payment3 = spilman_service
        .create_payment(&channel_id, 50)
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
    tracing::info!("Server accepted payment 3: balance={}", result3.balance);

    // ─── Phase 8: Verify channel state ───
    tracing::info!("Phase 8: Verifying channel state");
    let client_info = spilman_service
        .get_channel_info(&channel_id)
        .expect("client channel info");
    assert_eq!(client_info.current_balance, 50);
    assert_eq!(client_info.payment_count, 3);
    tracing::info!(
        "Client state: balance={} payments={} capacity={}",
        client_info.current_balance,
        client_info.payment_count,
        client_info.capacity,
    );

    let server_payment = server_bridge
        .host()
        .get_last_payment(&channel_id)
        .expect("server has last payment");
    assert_eq!(server_payment.balance, 50);
    tracing::info!("Server state: balance={}", server_payment.balance);

    // ─── Phase 9: Cooperative close ───
    tracing::info!("Phase 9: Cooperative close at balance=50 sat");
    let close_request = spilman_service
        .request_cooperative_close(&channel_id, 50)
        .expect("create cooperative close request");
    tracing::info!(
        "Close request: channel={} balance={}",
        &close_request.channel_id[..close_request.channel_id.len().min(16)],
        close_request.balance,
    );

    let server_net = ServerAsyncNetworking::new();
    let close_json = serde_json::to_string(&close_request).expect("serialize close request");
    let close_result = server_bridge
        .execute_cooperative_close_async(&close_json, &server_net)
        .await
        .expect("execute cooperative close");

    tracing::info!(
        "Cooperative close succeeded: receiver_sum={} sender_sum={} total={}",
        close_result.receiver_sum,
        close_result.sender_sum,
        close_result.total_value,
    );
    assert!(close_result.receiver_sum > 0, "receiver must get proofs");
    assert!(
        close_result.receiver_sum >= 50,
        "receiver must get at least the balance (got {})",
        close_result.receiver_sum,
    );
    assert!(
        close_result.sender_sum > 0,
        "sender must get refund proofs (capacity - balance - fees)"
    );

    // Client processes close response
    let close_response_json = serde_json::json!({
        "channel_id": close_result.channel_id,
    })
    .to_string();
    spilman_service
        .confirm_cooperative_close(&close_response_json)
        .expect("client processes close response");

    let final_info = spilman_service
        .get_channel_info(&channel_id)
        .expect("client channel info after close");
    assert_eq!(
        final_info.state,
        cdk_spilman::ClientChannelState::Closed,
        "channel must be closed after cooperative close"
    );
    tracing::info!("Client channel state after close: {:?}", final_info.state);

    // ─── Summary ───
    tracing::info!("");
    tracing::info!("=== SpilmanService Integration Test Complete ===");
    tracing::info!("Channel ID: {channel_id}");
    tracing::info!("Capacity: {} sat", open_result.capacity);
    tracing::info!("Payments: 3 (10, 25, 50 sat)");
    tracing::info!(
        "Cooperative close: receiver={} sat, sender refund={} sat",
        close_result.receiver_sum,
        close_result.sender_sum,
    );
    tracing::info!("SUCCESS: full SpilmanService lifecycle works against testnut!");
}
