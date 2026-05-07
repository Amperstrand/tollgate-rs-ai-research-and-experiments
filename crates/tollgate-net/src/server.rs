use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::post, Router};
use tokio::sync::Mutex;
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
    }
}
