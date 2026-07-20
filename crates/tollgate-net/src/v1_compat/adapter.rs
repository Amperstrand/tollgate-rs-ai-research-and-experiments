use std::net::IpAddr;

use sha2::Digest;
use tollgate_net::status::PeerStatus;
use tollgate_protocol::{BootstrapAck, BootstrapToken, MessageType, peek_type};

use crate::driver::Driver;
use crate::v1_compat::mac_resolver::{DhcpLeasesResolver, MacResolver};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_peer_hex_returns_none_without_dhcp() {
        let result = resolve_peer_hex(Some(std::net::IpAddr::V4([10, 99, 99, 99].into()))).await;
        assert!(
            result.is_none(),
            "resolve_peer_hex must return None when DHCP fails, got: {result:?}"
        );
    }

    #[test]
    fn mac_to_peer_hex_is_deterministic() {
        let mac = "aa:bb:cc:dd:ee:ff";
        let h1 = mac_to_peer_hex(mac);
        let h2 = mac_to_peer_hex(mac);
        assert_eq!(h1, h2, "same MAC must produce same peer_hex");
        assert!(!h1.is_empty(), "peer_hex must not be empty");
    }

    #[test]
    fn mac_to_peer_hex_format_invariant() {
        let colon = mac_to_peer_hex("aa:bb:cc:dd:ee:ff");
        let dash = mac_to_peer_hex("aa-bb-cc-dd-ee-ff");
        let bare = mac_to_peer_hex("aabbccddeeff");
        assert_eq!(colon, dash, "colon and dash formats must match");
        assert_eq!(colon, bare, "colon and bare formats must match");
    }

    #[test]
    fn mac_to_peer_hex_different_macs_produce_different_outputs() {
        let a = mac_to_peer_hex("00:00:00:00:00:01");
        let b = mac_to_peer_hex("00:00:00:00:00:02");
        assert_ne!(
            a, b,
            "different MACs must produce different peer_hex values"
        );
    }

    #[test]
    fn mac_to_peer_hex_output_is_66_char_hex() {
        let h = mac_to_peer_hex("01:23:45:67:89:ab");
        assert_eq!(
            h.len(),
            66,
            "compressed pubkey hex must be 66 chars (33 bytes), got {}",
            h.len()
        );
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit()),
            "must be valid hex"
        );
    }
}
