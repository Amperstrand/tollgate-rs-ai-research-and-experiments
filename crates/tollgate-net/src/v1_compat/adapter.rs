use std::net::IpAddr;

use sha2::Digest;
use tollgate_net::status::PeerStatus;
use tollgate_protocol::{BootstrapAck, BootstrapToken, MessageType, peek_type};

use crate::driver::Driver;
use crate::v1_compat::mac_resolver::{DhcpLeasesResolver, MacResolver, StubMacResolver};

pub fn mac_to_peer_hex(mac: &str) -> String {
    let clean = mac.replace([':', '-'], "");
    let seed = format!("tollgate-v1-compat/{clean}");
    let hash = sha2::Sha256::digest(seed.as_bytes());
    let secret = secp256k1::SecretKey::from_slice(&hash)
        .expect("SHA256 output is always a valid secp256k1 scalar");
    let secp = secp256k1::Secp256k1::new();
    let pubkey = secret.public_key(&secp);
    hex::encode(pubkey.serialize())
}

pub async fn resolve_peer_hex(ip: Option<IpAddr>) -> Option<String> {
    let ip = ip?;
    let ip_str = ip.to_string();
    let resolver = DhcpLeasesResolver;
    if let Ok(mac) = <DhcpLeasesResolver as MacResolver>::resolve(&resolver, &ip_str) {
        return Some(mac_to_peer_hex(&mac));
    }
    let stub = StubMacResolver::default();
    if let Ok(mac) = stub.resolve(&ip_str) {
        return Some(mac_to_peer_hex(&mac));
    }
    None
}

pub struct V1PaymentResult {
    pub accepted: bool,
    pub reason: Option<String>,
    pub peer_hex: String,
}

pub async fn admit_peer_with_token(
    driver: &Driver,
    peer_hex: &str,
    ip: Option<IpAddr>,
    token: &str,
) -> V1PaymentResult {
    driver.peer_connected(peer_hex, ip).await;

    let bootstrap_msg = BootstrapToken::new(token.as_bytes().to_vec()).encode();
    driver.message_received(peer_hex, bootstrap_msg).await;

    let outbox = driver.drain_outbox(peer_hex).await;

    for msg in &outbox {
        if matches!(peek_type(msg), Some(MessageType::BootstrapAck)) {
            if let Ok(ack) = BootstrapAck::decode(msg) {
                return V1PaymentResult {
                    accepted: ack.is_accepted(),
                    reason: ack.reason,
                    peer_hex: peer_hex.to_string(),
                };
            }
        }
    }

    V1PaymentResult {
        accepted: false,
        reason: Some("no response from gateway".to_string()),
        peer_hex: peer_hex.to_string(),
    }
}

pub async fn find_peer_status(driver: &Driver, peer_hex: &str) -> Option<PeerStatus> {
    let status = driver.status().await;
    status.peers.into_iter().find(|p| p.pubkey == peer_hex)
}

pub async fn get_usage_text(driver: &Driver, peer_hex: &str) -> Option<String> {
    let peer = find_peer_status(driver, peer_hex).await?;
    let delivered = peer.delivered;
    let balance = peer.their_balance.max(0) as u64;
    Some(format!("{delivered}/{balance}"))
}

pub async fn get_balance_json(driver: &Driver, peer_hex: &str) -> Option<serde_json::Value> {
    let peer = find_peer_status(driver, peer_hex).await?;
    let is_active = peer.state == "Active";
    Some(serde_json::json!({
        "balance": peer.their_balance,
        "remaining": peer.their_balance.max(0),
        "delivered": peer.delivered,
        "received": peer.received,
        "state": peer.state,
        "session_active": is_active,
        "metered_secs": peer.metered_secs,
    }))
}
