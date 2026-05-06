use std::sync::Arc;
use std::time::Duration;

use tollgate_core::protocol::{
    BootstrapStatus, BootstrapToken, Disconnect, Hash32, IntervalRange, Message, MeteringReport,
    MessageType, PubKey, ReasonCode, Accept,
};
use tollgate_core::session::{PeerSession, SessionConfig};

use crate::mock::{MockAdapter, MockWallet};

fn client_pubkey() -> PubKey {
    PubKey([0x02; 33])
}

fn client_config() -> SessionConfig {
    SessionConfig {
        pubkey: client_pubkey(),
        protocol_version: 1,
        unit: "bytes".to_owned(),
        capabilities: 0x01,
        products: vec![],
        interval_ms: 5000,
    }
}

fn make_token(amount: u64) -> Vec<u8> {
    amount.to_be_bytes().to_vec()
}

async fn send_message(
    client: &reqwest::Client,
    peer_url: &str,
    msg: &Message,
) -> Vec<Message> {
    let body = minicbor::to_vec(msg).expect("encode message");
    let resp = client
        .post(format!("{peer_url}/tollgate/message"))
        .header("content-type", "application/cbor")
        .body(body)
        .send()
        .await
        .expect("send message to provider");
    let bytes = resp.bytes().await.expect("read response body");
    if bytes.is_empty() {
        return vec![];
    }
    minicbor::decode(&bytes).expect("decode response")
}

fn extract_product_info(responses: &[Message]) -> Option<(Hash32, Hash32)> {
    for msg in responses {
        if let Message::PriceSheet(sheet) = msg {
            if let Some(product) = sheet.products.first() {
                if let Some(mint) = product.mint_options.first() {
                    return Some((product.product_id.clone(), mint.option_id.clone()));
                }
            }
        }
    }
    None
}

pub async fn run(peer_url: &str, intervals: u32, interval_secs: u64) {
    let http = reqwest::Client::new();

    let wallet = Arc::new(MockWallet::new(200));
    let adapter = Arc::new(MockAdapter::new());
    let config = client_config();
    let mut session = PeerSession::new(wallet, adapter, config);

    tracing::info!("Connecting to {peer_url}");

    let announce = session.create_announce();
    let responses = send_message(&http, peer_url, &announce).await;
    tracing::info!("Sent Announce");

    for msg in &responses {
        session.handle_message(msg.clone()).await;
    }

    let Some((product_id, option_id)) = extract_product_info(&responses) else {
        tracing::error!("No product found in provider PriceSheet");
        return;
    };
    tracing::info!("Received Announce + PriceSheet from provider");

    let accept = Message::Accept(Accept {
        msg_type: MessageType::Accept as u8,
        product_id,
        option_id,
        interval_range: IntervalRange([2500, 10000]),
        channel_funding: vec![],
    });
    let _ = send_message(&http, peer_url, &accept).await;
    tracing::info!("Selected product, sent Accept");

    let bootstrap_token = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: make_token(100),
    });
    let responses = send_message(&http, peer_url, &bootstrap_token).await;
    for msg in &responses {
        session.handle_message(msg.clone()).await;
    }

    let accepted = responses.iter().any(|m| {
        matches!(
            m,
            Message::BootstrapAck(ack) if ack.status == BootstrapStatus::Accepted
        )
    });
    if !accepted {
        tracing::error!("Bootstrap token rejected by provider");
        return;
    }
    tracing::info!("Sent BootstrapToken (100 sats) — accepted! Session active.");

    let mut elapsed_ms: u64 = 0;
    let mut delivered: u64 = 0;

    for i in 1..=intervals {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        elapsed_ms += interval_secs * 1000;
        delivered += 1000;

        let report = Message::MeteringReport(MeteringReport {
            msg_type: MessageType::MeteringReport as u8,
            elapsed_ms,
            delivered,
            received: delivered / 2,
            new_product_id: None,
            new_pricing: None,
        });

        let responses = send_message(&http, peer_url, &report).await;

        let mut balance_exhausted = false;
        for msg in &responses {
            if let Message::Reject(r) = msg {
                if r.reason_text.as_deref() == Some("balance exhausted") {
                    balance_exhausted = true;
                }
            }
            session.handle_message(msg.clone()).await;
        }

        if balance_exhausted {
            tracing::info!("[Interval {i}] BALANCE EXHAUSTED! Sending top-up...");
            let top_up = Message::BootstrapToken(BootstrapToken {
                msg_type: MessageType::BootstrapToken as u8,
                token: make_token(50),
            });
            let top_up_responses = send_message(&http, peer_url, &top_up).await;
            for msg in &top_up_responses {
                session.handle_message(msg.clone()).await;
            }
            tracing::info!("Top-up accepted (50 sats), access restored");
        } else {
            tracing::info!("[Interval {i}] delivered={delivered} units, balance OK");
        }
    }

    let disconnect = Message::Disconnect(Disconnect {
        msg_type: MessageType::Disconnect as u8,
        reason_code: ReasonCode::Other,
    });
    let _ = send_message(&http, peer_url, &disconnect).await;
    tracing::info!("Disconnecting. Session complete.");
}
