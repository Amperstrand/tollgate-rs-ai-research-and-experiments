use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::post, Router};
use tokio::sync::Mutex;
use tollgate_core::bootstrap::ExhaustionConfig;
use tollgate_core::config::{ProductConfig, ProductMintConfig};
use tollgate_core::metering::PeerMetrics;
use tollgate_core::protocol::{Hash32, Message, PubKey};
use tollgate_core::session::{PeerSession, SessionConfig};
use tollgate_core::wallet::Wallet;

use crate::mock::MockAdapter;

struct AppState<W: Wallet> {
    session: Mutex<PeerSession<W, MockAdapter>>,
    adapter: Arc<MockAdapter>,
}

fn provider_pubkey() -> PubKey {
    PubKey([0x01; 33])
}

fn test_product_id() -> Hash32 {
    Hash32([0x11; 32])
}

fn test_option_id() -> Hash32 {
    Hash32([0x22; 32])
}

pub fn provider_config() -> SessionConfig {
    SessionConfig {
        pubkey: provider_pubkey(),
        protocol_version: 1,
        unit: "bytes".to_owned(),
        capabilities: 0x01,
        products: vec![ProductConfig {
            product_id: test_product_id(),
            extensions: vec![],
            pricing_scale: 1000,
            mint_options: vec![ProductMintConfig {
                option_id: test_option_id(),
                mint_url: "https://mint.example.com".to_owned(),
                price_per_second: 10,
                price_per_unit: 1,
                mint_unit: "sat".to_owned(),
            }],
        }],
        interval_ms: 5000,
        min_checkin_ms: 1000,
        max_interval_ms: 10000,
        exhaustion: ExhaustionConfig::default(),
    }
}

#[allow(clippy::missing_panics_doc)]
pub async fn run<W: Wallet + 'static>(port: u16, wallet: Arc<W>) {
    let adapter = Arc::new(MockAdapter::new());
    let config = provider_config();
    let session = PeerSession::new(wallet, adapter.clone(), config);

    let state = Arc::new(AppState {
        session: Mutex::new(session),
        adapter,
    });

    let app = Router::new()
        .route("/tollgate/message", post(handle_message::<W>))
        .with_state(state);

    tracing::info!("Provider listening on port {port}");
    tracing::info!("Product: 10 sat/s + 1 sat/unit, scale=1000");

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind listener");
    axum::serve(listener, app)
        .await
        .expect("server exited with error");
}

async fn handle_message<W: Wallet>(
    State(state): State<Arc<AppState<W>>>,
    body: axum::body::Bytes,
) -> (StatusCode, Vec<u8>) {
    let msg: Message = match minicbor::decode(&body) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Failed to decode CBOR message: {e}");
            return (StatusCode::BAD_REQUEST, vec![]);
        }
    };

    if let Message::MeteringReport(ref report) = msg {
        state.adapter.set_metrics(PeerMetrics {
            elapsed_ms: report.elapsed_ms,
            delivered: report.delivered,
            received: report.received,
        });
    }

    let is_announce = matches!(msg, Message::Announce(_));
    let msg_name = message_name(&msg);

    let mut session = state.session.lock().await;
    let mut responses = session.handle_message(msg).await;

    tracing::info!("Received {msg_name}, state: {:?}", session.state());

    if is_announce && responses.is_empty() {
        tracing::info!("Sending Announce + PriceSheet to peer");
        responses.push(session.create_announce());
        responses.push(session.create_price_sheet());
    }

    log_responses(&responses);

    if responses.is_empty() {
        (StatusCode::OK, vec![])
    } else {
        match minicbor::to_vec(&responses) {
            Ok(bytes) => (StatusCode::OK, bytes),
            Err(e) => {
                tracing::error!("Failed to encode response: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, vec![])
            }
        }
    }
}

fn log_responses(responses: &[Message]) {
    for resp in responses {
        match resp {
            Message::BootstrapAck(ack) => {
                tracing::info!("BootstrapAck: {:?}", ack.status);
            }
            Message::Reject(r) => {
                tracing::info!(
                    "Reject: {}",
                    r.reason_text.as_deref().unwrap_or("(no reason)")
                );
            }
            Message::Disconnect(_) => {
                tracing::info!("Peer disconnected. Session closed.");
            }
            _ => {}
        }
    }
}

fn message_name(msg: &Message) -> &'static str {
    match msg {
        Message::Announce(_) => "Announce",
        Message::PriceSheet(_) => "PriceSheet",
        Message::Accept(_) => "Accept",
        Message::ChannelReady(_) => "ChannelReady",
        Message::MeteringReport(_) => "MeteringReport",
        Message::BalanceUpdate(_) => "BalanceUpdate",
        Message::BalanceAck(_) => "BalanceAck",
        Message::BootstrapToken(_) => "BootstrapToken",
        Message::BootstrapAck(_) => "BootstrapAck",
        Message::RolloverInit(_) => "RolloverInit",
        Message::RolloverReady(_) => "RolloverReady",
        Message::ChannelClose(_) => "ChannelClose",
        Message::CloseAck(_) => "CloseAck",
        Message::Reject(_) => "Reject",
        Message::Disconnect(_) => "Disconnect",
        Message::MeteringReportResponse(_) => "MeteringReportResponse",
    }
}

// ---------------------------------------------------------------------------
// Spilman payment channel server (requires --features spilman)
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
use {
    crate::spilman_service::{PaymentProof, SpilmanAsyncNetworking, SpilmanBridge, SpilmanHost},
    async_trait::async_trait,
    cashu::nuts::{CurrencyUnit, Id, Proof as CashuProof, PublicKey, SecretKey},
    cdk_spilman::{
        compute_channel_secret_from_hex, sign_with_tweaked_key_util, ChannelFunding, ChannelPolicy,
        ChannelState, ClosingData,
    },
    serde_json,
    std::collections::HashMap,
    std::sync::atomic::{AtomicU64, Ordering},
    std::time::{SystemTime, UNIX_EPOCH},
    tollgate_core::protocol::{BalanceAck, CloseAck, MessageType},
};

#[cfg(feature = "spilman")]
fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(feature = "spilman")]
struct ServerNetworking {
    client: reqwest::Client,
}

#[cfg(feature = "spilman")]
impl ServerNetworking {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "spilman")]
#[async_trait]
impl SpilmanAsyncNetworking for ServerNetworking {
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
            .map_err(|e| format!("server swap failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("server swap failed: {status} - {body}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("read swap response: {e}"))
    }

    async fn refresh_all_keysets(&self, mint: &str) -> Result<(), String> {
        tracing::info!("[server-net] keyset refresh skipped (mint={mint})");
        Ok(())
    }
}

#[cfg(feature = "spilman")]
struct ServerSpilmanHost {
    receiver_secret: SecretKey,
    channels: std::sync::Mutex<HashMap<String, ChannelFunding>>,
    payments: std::sync::Mutex<HashMap<String, PaymentProof>>,
    states: std::sync::Mutex<HashMap<String, ChannelState>>,
    closing_data: std::sync::Mutex<HashMap<String, ClosingData>>,
    keyset_infos: std::sync::Mutex<HashMap<Id, String>>,
    active_keysets: std::sync::Mutex<HashMap<String, Vec<Id>>>,
    amount_due: AtomicU64,
}

#[cfg(feature = "spilman")]
impl ServerSpilmanHost {
    fn new(receiver_secret: SecretKey) -> Self {
        Self {
            receiver_secret,
            channels: std::sync::Mutex::new(HashMap::new()),
            payments: std::sync::Mutex::new(HashMap::new()),
            states: std::sync::Mutex::new(HashMap::new()),
            closing_data: std::sync::Mutex::new(HashMap::new()),
            keyset_infos: std::sync::Mutex::new(HashMap::new()),
            active_keysets: std::sync::Mutex::new(HashMap::new()),
            amount_due: AtomicU64::new(0),
        }
    }

    #[allow(dead_code)]
    fn add_keyset(&self, mint_url: &str, keyset_id: Id, keyset_info_json: String) {
        self.keyset_infos
            .lock()
            .unwrap()
            .insert(keyset_id, keyset_info_json);
        self.active_keysets
            .lock()
            .unwrap()
            .entry(mint_url.to_string())
            .or_default()
            .push(keyset_id);
    }

    fn receiver_pubkey_hex(&self) -> String {
        self.receiver_secret.public_key().to_hex()
    }
}

#[cfg(feature = "spilman")]
impl SpilmanHost<()> for ServerSpilmanHost {
    fn receiver_key_is_acceptable(&self, pk: &PublicKey) -> bool {
        *pk == self.receiver_secret.public_key()
    }

    fn mint_and_keyset_is_acceptable(&self, _mint: &str, _id: &Id) -> bool {
        true
    }

    fn get_funding(&self, cid: &str) -> Option<ChannelFunding> {
        self.channels.lock().unwrap().get(cid).cloned()
    }

    fn save_funding(&self, cid: &str, funding: ChannelFunding, payment: PaymentProof) {
        self.channels
            .lock()
            .unwrap()
            .insert(cid.to_string(), funding);
        self.payments
            .lock()
            .unwrap()
            .insert(cid.to_string(), payment);
        self.states
            .lock()
            .unwrap()
            .insert(cid.to_string(), ChannelState::Open);
    }

    fn get_amount_due(&self, _cid: &str, _ctx: Option<&()>) -> u64 {
        self.amount_due.load(Ordering::SeqCst)
    }

    fn record_payment(&self, cid: &str, payment: PaymentProof, _ctx: &()) {
        self.payments
            .lock()
            .unwrap()
            .insert(cid.to_string(), payment.clone());
        self.amount_due.store(payment.balance, Ordering::SeqCst);
    }

    fn get_channel_state(&self, cid: &str) -> ChannelState {
        self.states
            .lock()
            .unwrap()
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
            .lock()
            .unwrap()
            .insert(cid.to_string(), ChannelState::Closing);
        self.closing_data.lock().unwrap().insert(
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
        self.closing_data.lock().unwrap().get(cid).cloned()
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
            .map_or(0, |d| d.as_secs())
    }

    fn get_balance_and_signature_for_unilateral_exit(&self, cid: &str) -> Option<PaymentProof> {
        self.payments.lock().unwrap().get(cid).cloned()
    }

    fn get_active_keyset_ids(&self, mint: &str, _unit: &CurrencyUnit) -> Vec<Id> {
        self.active_keysets
            .lock()
            .unwrap()
            .get(mint)
            .cloned()
            .unwrap_or_default()
    }

    fn get_keyset_info(&self, _mint: &str, kid: &Id) -> Option<String> {
        self.keyset_infos.lock().unwrap().get(kid).cloned()
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
            .lock()
            .unwrap()
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

#[cfg(feature = "spilman")]
#[allow(dead_code)]
struct SpilmanServerState {
    bridge: SpilmanBridge<ServerSpilmanHost, ()>,
    net: ServerNetworking,
    mint_url: String,
}

#[cfg(feature = "spilman")]
struct SpilmanAppState {
    session: Mutex<PeerSession<crate::cdk_wallet::CdkWallet, MockAdapter>>,
    adapter: Arc<MockAdapter>,
    spilman: Mutex<SpilmanServerState>,
}

#[cfg(feature = "spilman")]
#[allow(clippy::missing_panics_doc)]
pub async fn run_spilman(
    port: u16,
    wallet: Arc<crate::cdk_wallet::CdkWallet>,
    receiver_secret: SecretKey,
    mint_url: &str,
) {
    let adapter = Arc::new(MockAdapter::new());
    let config = provider_config();
    let session = PeerSession::new(wallet, adapter.clone(), config);

    let server_host = ServerSpilmanHost::new(receiver_secret);
    let receiver_pubkey_hex = server_host.receiver_pubkey_hex();
    let server_bridge = SpilmanBridge::new(server_host);

    let spilman_state = SpilmanServerState {
        bridge: server_bridge,
        net: ServerNetworking::new(),
        mint_url: mint_url.to_owned(),
    };

    let state = Arc::new(SpilmanAppState {
        session: Mutex::new(session),
        adapter,
        spilman: Mutex::new(spilman_state),
    });

    let app = Router::new()
        .route("/tollgate/message", post(handle_spilman_message))
        .route("/tollgate/force-close", post(handle_force_close))
        .with_state(state);

    tracing::info!("Spilman provider listening on port {port}");
    tracing::info!("Receiver pubkey: {receiver_pubkey_hex}");

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind listener");
    axum::serve(listener, app)
        .await
        .expect("server exited with error");
}

#[cfg(feature = "spilman")]
#[allow(clippy::too_many_lines)]
async fn handle_spilman_message(
    State(state): State<Arc<SpilmanAppState>>,
    body: axum::body::Bytes,
) -> (StatusCode, Vec<u8>) {
    let msg: Message = match minicbor::decode(&body) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Failed to decode CBOR message: {e}");
            return (StatusCode::BAD_REQUEST, vec![]);
        }
    };

    if let Message::MeteringReport(ref report) = msg {
        state.adapter.set_metrics(PeerMetrics {
            elapsed_ms: report.elapsed_ms,
            delivered: report.delivered,
            received: report.received,
        });
    }

    let is_announce = matches!(msg, Message::Announce(_));
    let msg_name = message_name(&msg);

    let mut session = state.session.lock().await;
    let mut responses = session.handle_message(msg.clone()).await;

    tracing::info!("Received {msg_name}, state: {:?}", session.state());

    if is_announce && responses.is_empty() {
        tracing::info!("Sending Announce + PriceSheet to peer");
        responses.push(session.create_announce());
        responses.push(session.create_price_sheet());
    }

    drop(session);

    if let Message::BalanceUpdate(ref update) = msg {
        let channel_id_hex = encode_hex(&update.channel_id.0);
        let signature_hex = encode_hex(&update.balance_signature.0);

        let params_val = update
            .channel_params_json
            .as_ref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let proofs_val: Option<Vec<CashuProof>> = update
            .funding_proofs_json
            .as_ref()
            .and_then(|b| serde_json::from_slice(b).ok());

        let spilman_state = state.spilman.lock().await;
        match spilman_state.bridge.process_payment(
            &channel_id_hex,
            update.cumulative_balance,
            &signature_hex,
            params_val.as_ref(),
            proofs_val.as_deref(),
            &(),
        ) {
            Ok(result) => {
                tracing::info!(
                    "[spilman] Payment accepted: channel={} balance={}",
                    &channel_id_hex[..channel_id_hex.len().min(16)],
                    result.balance,
                );
                responses.push(Message::BalanceAck(BalanceAck {
                    msg_type: MessageType::BalanceAck as u8,
                    channel_id: update.channel_id.clone(),
                    accepted_balance: result.balance,
                }));
            }
            Err(e) => {
                tracing::warn!("[spilman] Payment rejected: {e}");
                responses.push(Message::BalanceAck(BalanceAck {
                    msg_type: MessageType::BalanceAck as u8,
                    channel_id: update.channel_id.clone(),
                    accepted_balance: update.cumulative_balance,
                }));
            }
        }
    }

    if let Message::ChannelClose(ref close) = msg {
        let channel_id_hex = encode_hex(&close.channel_id.0);

        let close_params_val = close
            .channel_params_json
            .as_ref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let close_proofs_val: Option<Vec<CashuProof>> = close
            .funding_proofs_json
            .as_ref()
            .and_then(|b| serde_json::from_slice(b).ok());

        let _ = (close_params_val, close_proofs_val);

        tracing::info!(
            "[spilman] ChannelClose: channel={} balance={}",
            &channel_id_hex[..channel_id_hex.len().min(16)],
            close.final_balance,
        );
        responses.push(Message::CloseAck(CloseAck {
            msg_type: MessageType::CloseAck as u8,
            channel_id: close.channel_id.clone(),
            accepted_balance: close.final_balance,
        }));
    }

    log_responses(&responses);

    if responses.is_empty() {
        (StatusCode::OK, vec![])
    } else {
        match minicbor::to_vec(&responses) {
            Ok(bytes) => (StatusCode::OK, bytes),
            Err(e) => {
                tracing::error!("Failed to encode response: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, vec![])
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server-initiated unilateral close (requires --features spilman)
// ---------------------------------------------------------------------------

#[cfg(feature = "spilman")]
async fn handle_force_close(
    State(state): State<Arc<SpilmanAppState>>,
    body: axum::body::Bytes,
) -> (StatusCode, String) {
    let channel_id = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(v) => v["channel_id"].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON: {e}"));
        }
    };

    if channel_id.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing channel_id".to_string());
    }

    tracing::info!(
        "[spilman] Force-close requested: channel={}",
        &channel_id[..channel_id.len().min(16)],
    );

    let spilman_state = state.spilman.lock().await;
    match spilman_state
        .bridge
        .execute_unilateral_close_async(&channel_id, &spilman_state.net)
        .await
    {
        Ok(result) => {
            tracing::info!(
                "[spilman] Unilateral close settled: channel={} receiver_sum={} sender_sum={}",
                &channel_id[..channel_id.len().min(16)],
                result.receiver_sum,
                result.sender_sum,
            );
            (
                StatusCode::OK,
                serde_json::json!({
                    "status": "closed",
                    "channel_id": result.channel_id,
                    "receiver_sum": result.receiver_sum,
                    "sender_sum": result.sender_sum,
                })
                .to_string(),
            )
        }
        Err(e) => {
            tracing::warn!("[spilman] Unilateral close failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("close failed: {e}"),
            )
        }
    }
}
