//! API parity verification tests for tollgate-rs v1 server.
//!
//! These tests verify that the Rust v1 server matches the Go v1 TollGate HTTP
//! API contract point-for-point, ensuring drop-in replacement compatibility.
//!
//! Go v1 HTTP API contract:
//! - Port 2121
//! - GET /     → Nostr event kind 10021 (advertisement) as JSON
//! - POST /    → Cashu token → Nostr event kind 1022 (session) or 21023 (notice) as JSON
//! - GET /usage   → plain text "{elapsed}/{allotment}" or "-1/-1"
//! - GET /whoami  → plain text "mac={address}" or "mac=unknown"
//! - GET /balance → JSON {"status":1,"session_active":bool,...}
//! - All responses have CORS headers: Access-Control-Allow-Origin only for local/private origins

use std::net::SocketAddr;
use std::sync::Arc;

use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;
use reqwest::Client;
use tollgate_core::wallet::Wallet;
use tollgate_net::mock::MockWallet;

use tollgate_net::v1::server::handlers::build_router;
use tollgate_net::v1::server::{
    build_advertisement, AcceptedMint, InMemoryLightningQuoteStore, InMemorySessionStore,
    LightningQuoteRecord, MerchantProvider, MockMintQuoteWallet, QuoteState, ServerState, StubMacResolver, StubValve,
    V1ServerConfig,
};

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

fn test_config() -> V1ServerConfig {
    V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 60_000,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    }
}

fn test_config_bytes_metric() -> V1ServerConfig {
    V1ServerConfig {
        metric: "bytes".to_owned(),
        step_size: 22_020_096,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    }
}

fn test_config_zero_price() -> V1ServerConfig {
    V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 60_000,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 0,
            unit: "sat".to_owned(),
            min_steps: 0,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    }
}

/// Create a mock token where the first 8 bytes encode `amount_sats` as big-endian u64.
/// MockWallet reads the first 8 bytes as the amount.
fn mock_token(amount_sats: u64) -> Vec<u8> {
    amount_sats.to_be_bytes().to_vec()
}

/// Start a v1 server on a random port. Returns (base_url, join_handle, server_state).
async fn start_server(
    config: V1ServerConfig,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<ServerState>,
) {
    let wallet: Arc<dyn Wallet> = Arc::new(MockWallet::new(0));
    let merchant = Arc::new(MerchantProvider::new(wallet));
    let advertisement = build_advertisement(&config).unwrap();

    let state = Arc::new(ServerState {
        merchant,
        config,
        sessions: Arc::new(InMemorySessionStore::new()),
        mac_resolver: Arc::new(StubMacResolver::default()),
        valve: Arc::new(StubValve),
        mint_quote_wallet: None,
        lightning_quotes: Arc::new(InMemoryLightningQuoteStore::new()),
        advertisement,
    });

    let app = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("mock server error");
    });

    (base_url, handle, state)
}

/// Helper: make a payment and return the response body as an Event.
async fn pay(client: &Client, base_url: &str, amount_sats: u64) -> Event {
    let token = mock_token(amount_sats);
    let resp = client.post(base_url).body(token).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    Event::from_json(&body).unwrap()
}

/// Helper: abort server and await.
async fn stop_server(handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

// ===========================================================================
// GET / (Advertisement) — kind 10021
// ===========================================================================

// Go v1 reference: GET / returns kind 10021 advertisement
#[tokio::test]
async fn parity_get_advertisement_returns_kind_10021() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(10_021));

    stop_server(server).await;
}

// Go v1 reference: kind 10021 event has valid signature
#[tokio::test]
async fn parity_get_advertisement_valid_signature() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert!(
        event.verify_signature(),
        "advertisement signature must be valid"
    );

    stop_server(server).await;
}

// Go v1 reference: kind 10021 has ["metric", value] tag
#[tokio::test]
async fn parity_get_advertisement_has_metric_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let metric = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("metric") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert_eq!(metric.as_deref(), Some("milliseconds"));

    stop_server(server).await;
}

// Go v1 reference: kind 10021 has ["step_size", value] tag
#[tokio::test]
async fn parity_get_advertisement_has_step_size_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let step_size = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("step_size") {
            items.get(1).and_then(|s| s.parse::<u64>().ok())
        } else {
            None
        }
    });
    assert_eq!(step_size, Some(60_000));

    stop_server(server).await;
}

// Go v1 reference: kind 10021 has ["price_per_step", "cashu", price, unit, url, min_steps] tags
#[tokio::test]
async fn parity_get_advertisement_has_price_per_step_tags() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let pricing_tags: Vec<Vec<String>> = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("price_per_step"))
        .map(|tag| tag.as_slice().to_vec())
        .collect();

    assert_eq!(
        pricing_tags.len(),
        1,
        "should have exactly one price_per_step tag"
    );
    let tag = &pricing_tags[0];
    assert_eq!(tag[0], "price_per_step");
    assert_eq!(tag[1], "cashu");
    assert_eq!(tag[2], "1"); // price
    assert_eq!(tag[3], "sat"); // unit
    assert_eq!(tag[4], "https://testnut.cashu.exchange"); // url
    assert_eq!(tag[5], "1"); // min_steps

    stop_server(server).await;
}

// Go v1 reference: GET / has CORS header Access-Control-Allow-Origin only for local/private origins
#[tokio::test]
async fn parity_get_advertisement_has_cors_headers() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://192.168.1.1:8080")
    );

    stop_server(server).await;
}

// Go v1 reference: advertisement content type is application/json
#[tokio::test]
async fn parity_get_advertisement_json_content_type() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type header")
        .to_str()
        .unwrap();
    assert!(
        ct.contains("application/json"),
        "expected application/json, got: {ct}"
    );

    stop_server(server).await;
}

// Go v1 reference: advertisement pubkey matches server's nostr_keys
#[tokio::test]
async fn parity_get_advertisement_pubkey_matches_config() {
    let config = test_config();
    let expected_pubkey = config.nostr_keys.public_key();
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.pubkey, expected_pubkey);

    stop_server(server).await;
}

// Go v1 reference: multiple accepted mints produce multiple price_per_step tags
#[tokio::test]
async fn parity_get_advertisement_multiple_mints() {
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 60_000,
        accepted_mints: vec![
            AcceptedMint {
                url: "https://mint-a.example.com".to_owned(),
                price_per_step: 1,
                unit: "sat".to_owned(),
                min_steps: 1,
            },
            AcceptedMint {
                url: "https://mint-b.example.com".to_owned(),
                price_per_step: 2,
                unit: "sat".to_owned(),
                min_steps: 5,
            },
        ],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    let resp = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let pricing_count = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("price_per_step"))
        .count();
    assert_eq!(pricing_count, 2, "should have two price_per_step tags");

    stop_server(server).await;
}

// ===========================================================================
// POST / (Payment) — kind 1022 session / kind 21023 notice
// ===========================================================================

// Go v1 reference: POST / with Cashu token returns kind 1022 session event
#[tokio::test]
async fn parity_post_payment_returns_kind_1022() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    let resp = client.post(&base_url).header("Origin", "http://192.168.1.1:8080").body(token).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(1022));

    stop_server(server).await;
}

// Go v1 reference: kind 1022 session event has valid signature
#[tokio::test]
async fn parity_post_session_event_valid_signature() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;
    assert!(
        event.verify_signature(),
        "session event signature must be valid"
    );

    stop_server(server).await;
}

// Go v1 reference: kind 1022 has ["allotment", value] tag
#[tokio::test]
async fn parity_post_session_event_has_allotment_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;

    let allotment = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("allotment") {
            items.get(1).and_then(|s| s.parse::<u64>().ok())
        } else {
            None
        }
    });
    assert_eq!(allotment, Some(600_000)); // 10 sats * 1 sat/step * 60000 ms/step

    stop_server(server).await;
}

// Go v1 reference: kind 1022 has ["metric", value] tag
#[tokio::test]
async fn parity_post_session_event_has_metric_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;

    let metric = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("metric") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert_eq!(metric.as_deref(), Some("milliseconds"));

    stop_server(server).await;
}

// Go v1 reference: kind 1022 has ["device-identifier", "mac", mac_address] tag
#[tokio::test]
async fn parity_post_session_event_has_device_identifier_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;

    let di_tag = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("device-identifier") {
            Some(items.to_vec())
        } else {
            None
        }
    });
    assert!(di_tag.is_some(), "device-identifier tag must exist");
    let tag = di_tag.unwrap();
    assert_eq!(tag[0], "device-identifier");
    assert_eq!(tag[1], "mac");
    assert!(
        tag[2].contains(':'),
        "MAC address should contain colons: {}",
        tag[2]
    );

    stop_server(server).await;
}

// Go v1 reference: kind 1022 has ["p", customer_identifier] tag
#[tokio::test]
async fn parity_post_session_event_has_p_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;

    let p_tag = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("p") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert!(
        p_tag.is_some(),
        "session event must have a 'p' tag"
    );
    assert!(
        p_tag.as_ref().unwrap().contains(':'),
        "p tag should contain MAC address with colons: {}",
        p_tag.unwrap()
    );

    stop_server(server).await;
}

// Go v1 reference: kind 1022 has ["start-time", unix_timestamp] tag
#[tokio::test]
async fn parity_post_session_event_has_start_time_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let event = pay(&client, &base_url, 10).await;
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let start_time = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("start-time") {
            items.get(1).and_then(|s| s.parse::<i64>().ok())
        } else {
            None
        }
    });
    assert!(start_time.is_some(), "session event must have start-time tag");
    let st = start_time.unwrap();
    assert!(
        st >= before && st <= after,
        "start-time {st} should be between {before} and {after}"
    );

    stop_server(server).await;
}

// Go v1 reference: X-Forwarded-For header is respected for IP resolution
#[tokio::test]
async fn parity_post_respects_x_forwarded_for() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    let resp = client
        .post(&base_url)
        .header("x-forwarded-for", "1.2.3.4")
        .body(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(1022));

    stop_server(server).await;
}

// Go v1 reference: GET /whoami respects X-Real-IP header
#[tokio::test]
async fn parity_whoami_respects_x_real_ip() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/whoami"))
        .header("x-real-ip", "1.2.3.4")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("mac="));

    stop_server(server).await;
}

// Go v1 reference: POST / with Nostr event wrapper (kind 21000) with "payment" tag works
#[tokio::test]
async fn parity_post_payment_with_nostr_event_wrapper() {
    let config = test_config();
    let tollgate_pubkey = config.nostr_keys.public_key();
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    let client_keys = Keys::generate();

    // MockWallet reads first 8 bytes of token string as BE u64. Any 8-byte
    // ASCII string produces a huge value that overflows steps * step_size.
    // Instead, send a short string (< 8 bytes) to prove the Nostr wrapper
    // extraction works: MockWallet rejects it → 400 with payment-error-invalid-token.
    let token_str = "short";
    let payment_event_json2 = wrap_token_event_json(&client_keys, tollgate_pubkey, token_str);

    let resp = client
        .post(&base_url)
        .body(payment_event_json2)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "short token from wrapper should be rejected"
    );

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(
        event.kind,
        Kind::Custom(21_023),
        "should get notice event for rejected token"
    );

    let code = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("code") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert_eq!(code.as_deref(), Some("payment-error-invalid-token"));

    stop_server(server).await;
}

/// Build a kind 21000 payment event JSON directly.
fn wrap_token_event_json(keys: &Keys, tollgate_pubkey: PublicKey, token: &str) -> String {
    let tags = Tags::from_list(vec![
        Tag::custom(TagKind::Custom("p".into()), [tollgate_pubkey.to_hex()]),
        Tag::custom(
            TagKind::Custom("device-identifier".into()),
            ["mac", "00:11:22:33:44:55"],
        ),
        Tag::custom(TagKind::Custom("payment".into()), [token.to_owned()]),
    ]);
    let event = EventBuilder::new(Kind::Custom(21_000), "")
        .tags(tags)
        .sign_with_keys(keys)
        .unwrap();
    event.as_json()
}

// Go v1 reference: POST / with raw token (no Nostr wrapper) works
#[tokio::test]
async fn parity_post_payment_raw_token() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // Send raw bytes — not a JSON event, just the mock token
    let token = mock_token(10);
    let resp = client.post(&base_url).header("Origin", "http://192.168.1.1:8080").body(token).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(1022));

    stop_server(server).await;
}

// Go v1 reference: invalid payment returns 400 with kind 21023 notice event
#[tokio::test]
async fn parity_post_invalid_payment_returns_kind_21023() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    assert_eq!(resp.status(), 400);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(21_023));

    stop_server(server).await;
}

// Go v1 reference: kind 21023 notice event has ["level", value] tag
#[tokio::test]
async fn parity_post_notice_has_level_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let has_level = event.tags.iter().any(|tag| {
        let items = tag.as_slice();
        items.first().map(String::as_str) == Some("level")
    });
    assert!(has_level, "notice event must have level tag");

    stop_server(server).await;
}

// Go v1 reference: kind 21023 notice event has ["code", value] tag
#[tokio::test]
async fn parity_post_notice_has_code_tag() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let has_code = event.tags.iter().any(|tag| {
        let items = tag.as_slice();
        items.first().map(String::as_str) == Some("code")
    });
    assert!(has_code, "notice event must have code tag");

    stop_server(server).await;
}

// Go v1 reference: kind 21023 notice event has error message in content
#[tokio::test]
async fn parity_post_notice_has_content_message() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert!(
        !event.content.is_empty(),
        "notice event content should not be empty"
    );

    stop_server(server).await;
}

// Go v1 reference: POST / has CORS headers
#[tokio::test]
async fn parity_post_payment_has_cors_headers() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    let resp = client.post(&base_url).header("Origin", "http://192.168.1.1:8080").body(token).send().await.unwrap();
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://192.168.1.1:8080")
    );

    stop_server(server).await;
}

// Go v1 reference: duplicate payment (same MAC) extends session (allotment stacks)
#[tokio::test]
async fn parity_post_duplicate_payment_extends_session() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // First payment: 10 sats → 600,000 ms allotment
    let event1 = pay(&client, &base_url, 10).await;
    let allotment1 = extract_allotment(&event1);
    assert_eq!(allotment1, Some(600_000));

    // Second payment: 5 sats → 300,000 ms additional
    let event2 = pay(&client, &base_url, 5).await;
    let allotment2 = extract_allotment(&event2);
    // Session allotment should be 600,000 + 300,000 = 900,000
    assert_eq!(
        allotment2,
        Some(900_000),
        "second payment should stack allotment"
    );

    stop_server(server).await;
}

// Go v1 reference: session event pubkey matches server's nostr_keys
#[tokio::test]
async fn parity_post_session_event_pubkey_matches_server() {
    let config = test_config();
    let expected_pubkey = config.nostr_keys.public_key();
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;
    assert_eq!(event.pubkey, expected_pubkey);

    stop_server(server).await;
}

// Go v1 reference: session event content is empty string
#[tokio::test]
async fn parity_post_session_event_empty_content() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;
    assert_eq!(event.content, "");

    stop_server(server).await;
}

// Go v1 reference: token too short (less than 8 bytes) is rejected
#[tokio::test]
async fn parity_post_short_token_rejected() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(&base_url)
        .body(vec![0u8, 1, 2]) // 3 bytes — too short for MockWallet
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(21_023));

    stop_server(server).await;
}

// Go v1 reference: zero-amount token is rejected
#[tokio::test]
async fn parity_post_zero_amount_token_rejected() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(&base_url)
        .body(mock_token(0))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(21_023));

    stop_server(server).await;
}

// ===========================================================================
// GET /usage — plain text "{elapsed}/{allotment}" or "-1/-1"
// ===========================================================================

// Go v1 reference: active session returns "{elapsed}/{allotment}" text format (NOT JSON)
#[tokio::test]
async fn parity_usage_active_session_text_format() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    // Should be "N/M" format, not JSON
    assert!(
        !body.trim().starts_with('{'),
        "usage should be plain text, not JSON: {body}"
    );
    let parts: Vec<&str> = body.trim().split('/').collect();
    assert_eq!(parts.len(), 2, "usage format should be elapsed/allotment");

    let usage: i64 = parts[0].parse().unwrap();
    let allotment: i64 = parts[1].parse().unwrap();
    assert!(usage >= 0, "elapsed should be non-negative");
    assert_eq!(allotment, 600_000);

    stop_server(server).await;
}

// Go v1 reference: no session returns "-1/-1"
#[tokio::test]
async fn parity_usage_no_session_returns_negative() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert_eq!(body.trim(), "-1/-1");

    stop_server(server).await;
}

// Go v1 reference: expired session (milliseconds metric) returns "-1/-1"
#[tokio::test]
async fn parity_usage_expired_session_returns_negative() {
    // Create config with very small allotment: 1ms step, 1 sat/step
    // A 1-sat payment → 1 step * 1ms = 1ms allotment, which expires almost immediately
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 1, // 1 millisecond per step
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    // Pay 1 sat → 1 step * 1ms = 1ms allotment
    pay(&client, &base_url, 1).await;

    // Wait long enough for the 1ms session to expire
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body.trim(), "-1/-1", "expired session should return -1/-1");

    stop_server(server).await;
}

// Go v1 reference: session is cleaned up after expiry check on /usage
#[tokio::test]
async fn parity_usage_expired_session_cleaned_up() {
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 1,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, state) = start_server(config).await;
    let client = Client::new();

    pay(&client, &base_url, 1).await;

    // Wait for expiry
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // /usage triggers cleanup
    client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();

    // Verify session is removed from store
    let session = state.sessions.get("00:11:22:33:44:55").await.unwrap();
    assert!(
        session.is_none(),
        "session should be cleaned up after expiry"
    );

    stop_server(server).await;
}

// Go v1 reference: /usage has CORS headers
#[tokio::test]
async fn parity_usage_has_cors_headers() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://192.168.1.1:8080")
    );

    stop_server(server).await;
}

// Go v1 reference: /usage for bytes metric shows 0/{allotment} (no time-based tracking)
#[tokio::test]
async fn parity_usage_bytes_metric_shows_zero_usage() {
    let (base_url, server, _state) = start_server(test_config_bytes_metric()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();

    // For bytes metric, usage is always 0 (server doesn't track bytes)
    let parts: Vec<&str> = body.trim().split('/').collect();
    assert_eq!(parts[0], "0");

    stop_server(server).await;
}

// ===========================================================================
// GET /whoami — plain text "mac={address}" or "mac=unknown"
// ===========================================================================

// Go v1 reference: Returns "mac={address}" text format (NOT JSON)
#[tokio::test]
async fn parity_whoami_returns_mac_text_format() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/whoami"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert!(
        !body.trim().starts_with('{'),
        "whoami should be plain text, not JSON: {body}"
    );
    assert!(body.starts_with("mac="));
    assert!(
        body.contains("00:11:22:33:44:55"),
        "should contain StubMacResolver's MAC: {body}"
    );

    stop_server(server).await;
}

// Go v1 reference: /whoami has CORS headers
#[tokio::test]
async fn parity_whoami_has_cors_headers() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/whoami"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://192.168.1.1:8080")
    );

    stop_server(server).await;
}

// ===========================================================================
// GET /balance — JSON with exact Go v1 fields
// ===========================================================================

// Go v1 reference: active session returns JSON with exact fields
#[tokio::test]
async fn parity_balance_active_session_has_all_fields() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Go v1 fields: status, session_active, metric, usage, allotment, remaining, start_time
    assert!(json.get("status").is_some(), "missing status");
    assert!(
        json.get("session_active").is_some(),
        "missing session_active"
    );
    assert!(json.get("metric").is_some(), "missing metric");
    assert!(json.get("usage").is_some(), "missing usage");
    assert!(json.get("allotment").is_some(), "missing allotment");
    assert!(json.get("remaining").is_some(), "missing remaining");
    assert!(json.get("start_time").is_some(), "missing start_time");

    stop_server(server).await;
}

// Go v1 reference: active session balance values are correct
#[tokio::test]
async fn parity_balance_active_session_values() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], true);
    assert_eq!(json["metric"], "milliseconds");
    assert_eq!(json["allotment"], 600_000);
    assert!(
        json["remaining"].as_u64().unwrap() > 0,
        "remaining should be positive for fresh session"
    );
    assert!(
        json["start_time"].as_i64().unwrap() > 0,
        "start_time should be positive unix timestamp"
    );
    assert_eq!(
        json["usage"].as_u64().unwrap(),
        0,
        "usage should be ~0 for fresh session"
    );

    stop_server(server).await;
}

// Go v1 reference: no session returns {"status":1,"session_active":false}
#[tokio::test]
async fn parity_balance_no_session() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], false);

    stop_server(server).await;
}

// Go v1 reference: expired session returns {"status":1,"session_active":false}
#[tokio::test]
async fn parity_balance_expired_session_returns_inactive() {
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 1,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    pay(&client, &base_url, 1).await;

    // Wait for expiry
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], false);

    stop_server(server).await;
}

// Go v1 reference: expired session is cleaned up after /balance check
#[tokio::test]
async fn parity_balance_expired_session_cleaned_up() {
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 1,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, state) = start_server(config).await;
    let client = Client::new();

    pay(&client, &base_url, 1).await;

    // Wait for expiry
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // /balance triggers cleanup
    client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();

    let session = state.sessions.get("00:11:22:33:44:55").await.unwrap();
    assert!(
        session.is_none(),
        "expired session should be cleaned up by /balance"
    );

    stop_server(server).await;
}

// Go v1 reference: /balance has CORS headers
#[tokio::test]
async fn parity_balance_has_cors_headers() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("http://192.168.1.1:8080")
    );

    stop_server(server).await;
}

// Go v1 reference: /balance returns application/json content type
#[tokio::test]
async fn parity_balance_json_content_type() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type header")
        .to_str()
        .unwrap();
    assert!(
        ct.contains("application/json"),
        "expected application/json, got: {ct}"
    );

    stop_server(server).await;
}

// ===========================================================================
// Edge cases: cross-endpoint interactions
// ===========================================================================

// Go v1 reference: payment then immediate usage check: allotment correct
#[tokio::test]
async fn parity_edge_payment_then_usage_check() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // Pay 10 sats → 10 steps * 60000ms = 600,000ms
    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let parts: Vec<&str> = body.trim().split('/').collect();
    let allotment: i64 = parts[1].parse().unwrap();
    assert_eq!(allotment, 600_000);

    stop_server(server).await;
}

// Go v1 reference: payment then balance check: all fields populated
#[tokio::test]
async fn parity_edge_payment_then_balance_check() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], true);
    assert_eq!(json["metric"], "milliseconds");
    assert_eq!(json["allotment"], 600_000);
    assert!(json["remaining"].as_u64().unwrap() <= 600_000);
    assert!(json["start_time"].as_i64().unwrap() > 0);

    stop_server(server).await;
}

// Go v1 reference: multiple payments same MAC: allotment accumulates
#[tokio::test]
async fn parity_edge_multiple_payments_accumulate() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // Pay 10 sats → 600,000ms
    let event1 = pay(&client, &base_url, 10).await;
    assert_eq!(extract_allotment(&event1), Some(600_000));

    // Pay 5 sats → additional 300,000ms
    let event2 = pay(&client, &base_url, 5).await;
    assert_eq!(extract_allotment(&event2), Some(900_000));

    // Pay 2 sats → additional 120,000ms
    let event3 = pay(&client, &base_url, 2).await;
    assert_eq!(extract_allotment(&event3), Some(1_020_000));

    // Verify /usage shows accumulated total
    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let parts: Vec<&str> = body.trim().split('/').collect();
    let allotment: i64 = parts[1].parse().unwrap();
    assert_eq!(allotment, 1_020_000);

    stop_server(server).await;
}

// Go v1 reference: server starts with clean state (no stale sessions)
#[tokio::test]
async fn parity_edge_clean_start_state() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // No payment yet — all endpoints should report no session
    let usage = client
        .get(format!("{base_url}/usage"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(usage.trim(), "-1/-1");

    let balance = client
        .get(format!("{base_url}/balance"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&balance).unwrap();
    assert_eq!(json["session_active"], false);

    stop_server(server).await;
}

// Go v1 reference: allotment calculation with price_per_step = 0 gives step_size
#[tokio::test]
async fn parity_edge_zero_price_allotment() {
    let (base_url, server, _state) = start_server(test_config_zero_price()).await;
    let client = Client::new();

    // price_per_step=0 → any non-zero amount gives step_size allotment
    let event = pay(&client, &base_url, 1).await;
    let allotment = extract_allotment(&event);
    assert_eq!(
        allotment,
        Some(60_000),
        "zero price should give step_size allotment"
    );

    stop_server(server).await;
}

// Go v1 reference: notice event has valid signature
#[tokio::test]
async fn parity_edge_notice_valid_signature() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert!(
        event.verify_signature(),
        "notice event signature must be valid"
    );

    stop_server(server).await;
}

// Go v1 reference: notice event pubkey matches server's nostr_keys
#[tokio::test]
async fn parity_edge_notice_pubkey_matches_server() {
    let config = test_config();
    let expected_pubkey = config.nostr_keys.public_key();
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.pubkey, expected_pubkey);

    stop_server(server).await;
}

// Go v1 reference: /balance remaining = allotment - usage
#[tokio::test]
async fn parity_edge_balance_remaining_calculation() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let usage = json["usage"].as_u64().unwrap();
    let allotment = json["allotment"].as_u64().unwrap();
    let remaining = json["remaining"].as_u64().unwrap();

    assert_eq!(
        remaining,
        allotment - usage,
        "remaining should equal allotment - usage"
    );

    stop_server(server).await;
}

// Go v1 reference: advertisement is consistent across multiple GET / requests
#[tokio::test]
async fn parity_edge_advertisement_consistent() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp1 = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body1 = resp1.text().await.unwrap();

    let resp2 = client.get(&base_url).header("Origin", "http://192.168.1.1:8080").send().await.unwrap();
    let body2 = resp2.text().await.unwrap();

    assert_eq!(
        body1, body2,
        "advertisement should be identical across requests"
    );

    stop_server(server).await;
}

// Go v1 reference: after payment, /whoami returns the same MAC as the session
#[tokio::test]
async fn parity_edge_whoami_consistent_with_session() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let event = pay(&client, &base_url, 10).await;
    let session_mac = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("device-identifier") {
            items.get(2).cloned()
        } else {
            None
        }
    });

    let whoami = client
        .get(format!("{base_url}/whoami"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let whoami_mac = whoami.trim().strip_prefix("mac=").unwrap();
    assert_eq!(
        session_mac.as_deref(),
        Some(whoami_mac),
        "whoami MAC should match session device-identifier"
    );

    stop_server(server).await;
}

// Go v1 reference: error notice contains "payment-error-invalid-token" code for invalid tokens
#[tokio::test]
async fn parity_edge_notice_error_code_token_rejected() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let code = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("code") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert_eq!(
        code.as_deref(),
        Some("payment-error-invalid-token"),
        "error code should be payment-error-invalid-token"
    );

    stop_server(server).await;
}

// Go v1 reference: error notice level is "error" for rejected tokens
#[tokio::test]
async fn parity_edge_notice_level_error() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();

    let level = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("level") {
            items.get(1).cloned()
        } else {
            None
        }
    });
    assert_eq!(level.as_deref(), Some("error"), "level should be error");

    stop_server(server).await;
}

// Go v1 reference: balance endpoint returns JSON for no-session case
#[tokio::test]
async fn parity_edge_balance_no_session_is_json() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type")
        .to_str()
        .unwrap();
    assert!(
        ct.contains("application/json"),
        "balance should always return JSON"
    );

    let body = resp.text().await.unwrap();
    let _: serde_json::Value = serde_json::from_str(&body).expect("body should be valid JSON");

    stop_server(server).await;
}

// Go v1 reference: /usage for bytes metric never expires (no time-based cleanup)
#[tokio::test]
async fn parity_edge_bytes_metric_no_expiry_on_usage() {
    let (base_url, server, _state) = start_server(test_config_bytes_metric()).await;
    let client = Client::new();

    pay(&client, &base_url, 10).await;

    // Even after time passes, bytes metric sessions don't expire
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let parts: Vec<&str> = body.trim().split('/').collect();
    assert_ne!(
        parts[1], "-1",
        "bytes metric session should not expire based on time"
    );

    stop_server(server).await;
}

// Go v1 reference: after session expires and is cleaned, balance shows no session
#[tokio::test]
async fn parity_edge_expiry_then_balance_no_session() {
    let config = V1ServerConfig {
        metric: "milliseconds".to_owned(),
        step_size: 1,
        accepted_mints: vec![AcceptedMint {
            url: "https://testnut.cashu.exchange".to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        nostr_keys: Keys::generate(),
        port: 0,
    };
    let (base_url, server, _state) = start_server(config).await;
    let client = Client::new();

    pay(&client, &base_url, 1).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // /usage triggers cleanup
    let usage_body = client
        .get(format!("{base_url}/usage"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(usage_body.trim(), "-1/-1");

    // Now /balance should also show no session
    let balance_body = client
        .get(format!("{base_url}/balance"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&balance_body).unwrap();
    assert_eq!(json["session_active"], false);

    stop_server(server).await;
}

// Go v1 reference: all endpoints return 200 even for error conditions (except POST errors → 400)
#[tokio::test]
async fn parity_edge_status_codes_correct() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    // GET / → 200
    assert_eq!(client.get(&base_url).send().await.unwrap().status(), 200);

    // GET /usage → 200 (even with no session)
    assert_eq!(
        client
            .get(format!("{base_url}/usage"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // GET /whoami → 200
    assert_eq!(
        client
            .get(format!("{base_url}/whoami"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // GET /balance → 200
    assert_eq!(
        client
            .get(format!("{base_url}/balance"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // POST / with bad token → 400
    assert_eq!(
        client
            .post(&base_url)
            .body("bad")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    // POST / with valid token → 200
    assert_eq!(
        client
            .post(&base_url)
            .body(mock_token(5))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    stop_server(server).await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_allotment(event: &Event) -> Option<u64> {
    event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("allotment") {
            items.get(1).and_then(|s| s.parse::<u64>().ok())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// LN Invoice helpers
// ---------------------------------------------------------------------------

async fn start_server_with_ln(
    config: V1ServerConfig,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<ServerState>,
    Arc<MockMintQuoteWallet>,
) {
    let wallet: Arc<dyn Wallet> = Arc::new(MockWallet::new(0));
    let merchant = Arc::new(MerchantProvider::new(wallet));
    let advertisement = build_advertisement(&config).unwrap();
    let mint_quote_wallet = Arc::new(MockMintQuoteWallet::new());
    let lightning_quotes = Arc::new(InMemoryLightningQuoteStore::new());

    let state = Arc::new(ServerState {
        merchant,
        config,
        sessions: Arc::new(InMemorySessionStore::new()),
        mac_resolver: Arc::new(StubMacResolver::default()),
        valve: Arc::new(StubValve),
        mint_quote_wallet: Some(mint_quote_wallet.clone()),
        lightning_quotes,
        advertisement,
    });

    let app = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("mock server error");
    });

    (base_url, handle, state, mint_quote_wallet)
}

// ===========================================================================
// POST /ln-invoice
// ===========================================================================

#[tokio::test]
async fn parity_ln_invoice_post_returns_quote_and_unpaid_state() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 1);
    assert_eq!(body["state"], "UNPAID");
    assert_eq!(body["access_granted"], false);
    assert!(body["quote"].as_str().unwrap().starts_with("mock-quote-"));
    assert!(body["invoice"].as_str().unwrap().starts_with("lnbc"));
    assert_eq!(body["mint_url"], "https://testnut.cashu.exchange");
    assert_eq!(body["amount"], 10);
    assert!(body["expiry"].as_u64().unwrap() > 0);

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_post_accepts_mint_field_alias() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 1);
    assert_eq!(body["state"], "UNPAID");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_post_missing_amount_returns_error() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["error"], "amount and mint_url are required");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_post_unknown_mint_returns_error() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let resp = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://unknown.mint.example.com"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["error"], "mint not accepted");

    stop_server(server).await;
}

// ===========================================================================
// GET /ln-invoice
// ===========================================================================

#[tokio::test]
async fn parity_ln_invoice_get_unpaid_returns_invoice() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let post_resp: serde_json::Value = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let quote_id = post_resp["quote"].as_str().unwrap();

    let resp = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 1);
    assert_eq!(body["state"], "UNPAID");
    assert_eq!(body["access_granted"], false);
    assert_eq!(body["quote"], quote_id);
    assert_eq!(body["mint_url"], "https://testnut.cashu.exchange");
    assert_eq!(body["amount"], 10);

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_get_paid_grants_access() {
    let (base_url, server, state, mock_wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let post_resp: serde_json::Value = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let quote_id = post_resp["quote"].as_str().unwrap();
    mock_wallet
        .set_quote_state(quote_id, QuoteState::Paid)
        .await;

    let mut record = state.lightning_quotes.get(quote_id).await.unwrap().unwrap();
    record.cached_state_at = Some(0);
    state
        .lightning_quotes
        .update(quote_id, record)
        .await
        .unwrap();

    let resp = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 1);
    assert_eq!(body["state"], "ISSUED");
    assert_eq!(body["access_granted"], true);
    assert_eq!(body["allotment"], 600_000);
    assert_eq!(body["metric"], "milliseconds");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_get_unknown_quote_returns_not_found() {
    let (base_url, server, _state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/ln-invoice?quote=nonexistent"))
        .send()
        .await
        .unwrap();
    // 200 (not 404): busybox wget drops bodies on non-2xx, so the v1 protocol
    // returns errors as 200 + {"status":0,"error":...}.
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["error"], "quote not found");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_get_wrong_mac_returns_not_found() {
    let (base_url, server, state, _wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let record = LightningQuoteRecord {
        quote_id: "other-mac-quote".to_owned(),
        mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
        mint_url: "https://testnut.cashu.exchange".to_owned(),
        amount: 10,
        expiry: 1_700_000_000,
        allotment: 0,
        created_at: 1_000_000,
        completed_at: None,
        session_granted: false,
        processing: false,
        invoice: "lnbc10mock".to_owned(),
        cached_state: Some(QuoteState::Unpaid),
        cached_state_at: Some(1_000_000),
    };
    state.lightning_quotes.insert(record).await.unwrap();

    let resp = client
        .get(format!("{base_url}/ln-invoice?quote=other-mac-quote"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["error"], "quote not found");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_get_paid_creates_session() {
    let (base_url, server, state, mock_wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let post_resp: serde_json::Value = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let quote_id = post_resp["quote"].as_str().unwrap();
    mock_wallet
        .set_quote_state(quote_id, QuoteState::Paid)
        .await;

    let mut record = state.lightning_quotes.get(quote_id).await.unwrap().unwrap();
    record.cached_state_at = Some(0);
    state
        .lightning_quotes
        .update(quote_id, record)
        .await
        .unwrap();

    let _resp = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();

    let session = state
        .sessions
        .get("00:11:22:33:44:55")
        .await
        .unwrap()
        .expect("session should exist");
    assert_eq!(session.allotment, 600_000);
    assert_eq!(session.metric, "milliseconds");

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_get_after_granted_returns_cached() {
    let (base_url, server, state, mock_wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    let post_resp: serde_json::Value = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let quote_id = post_resp["quote"].as_str().unwrap();
    mock_wallet
        .set_quote_state(quote_id, QuoteState::Paid)
        .await;

    let mut record = state.lightning_quotes.get(quote_id).await.unwrap().unwrap();
    record.cached_state_at = Some(0);
    state
        .lightning_quotes
        .update(quote_id, record)
        .await
        .unwrap();

    let resp1 = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);
    let body1: serde_json::Value = resp1.json().await.unwrap();
    assert_eq!(body1["access_granted"], true);

    let resp2 = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert_eq!(body2["access_granted"], true);
    assert_eq!(body2["state"], "ISSUED");
    assert_eq!(body2["allotment"], 600_000);

    stop_server(server).await;
}

#[tokio::test]
async fn parity_ln_invoice_full_lifecycle() {
    let (base_url, server, state, mock_wallet) = start_server_with_ln(test_config()).await;
    let client = Client::new();

    // Step 1: POST /ln-invoice → get UNPAID quote
    let post_resp: serde_json::Value = client
        .post(format!("{base_url}/ln-invoice"))
        .json(&serde_json::json!({
            "amount": 10,
            "mint_url": "https://testnut.cashu.exchange"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(post_resp["status"], 1);
    assert_eq!(post_resp["state"], "UNPAID");
    assert_eq!(post_resp["access_granted"], false);
    assert!(!post_resp["quote"].as_str().unwrap().is_empty());
    assert!(post_resp["invoice"].as_str().unwrap().starts_with("lnbc"));

    let quote_id = post_resp["quote"].as_str().unwrap();

    // Step 2: GET /ln-invoice?quote=X → still UNPAID
    let get_resp: serde_json::Value = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(get_resp["status"], 1);
    assert_eq!(get_resp["state"], "UNPAID");
    assert_eq!(get_resp["access_granted"], false);

    // Step 3: Simulate Lightning payment — set quote to Paid
    mock_wallet
        .set_quote_state(quote_id, QuoteState::Paid)
        .await;

    // Invalidate cache so GET handler re-checks state
    let mut record = state.lightning_quotes.get(quote_id).await.unwrap().unwrap();
    record.cached_state_at = Some(0);
    state
        .lightning_quotes
        .update(quote_id, record)
        .await
        .unwrap();

    // Step 4: GET /ln-invoice?quote=X → ISSUED, access_granted=true
    let paid_resp = client
        .get(format!("{base_url}/ln-invoice?quote={quote_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(paid_resp.status(), 200);
    let paid_body: serde_json::Value = paid_resp.json().await.unwrap();
    assert_eq!(paid_body["status"], 1);
    assert_eq!(paid_body["state"], "ISSUED");
    assert_eq!(paid_body["access_granted"], true);
    // 10 sats × 1 sat/step × 60000ms/step = 600000 ms allotment
    assert_eq!(paid_body["allotment"], 600_000);
    assert_eq!(paid_body["metric"], "milliseconds");

    // Step 5: Verify session exists
    let mac = "00:11:22:33:44:55";
    let session = state
        .sessions
        .get(mac)
        .await
        .expect("session store error")
        .expect("session should exist");
    assert_eq!(session.allotment, 600_000);
    assert_eq!(session.metric, "milliseconds");

    // Step 6: GET /balance → shows active session
    let balance_resp = client
        .get(format!("{base_url}/balance"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(balance_resp.status(), 200);
    let balance: serde_json::Value = balance_resp.json().await.unwrap();
    assert_eq!(balance["status"], 1);
    assert_eq!(balance["session_active"], true);

    // Step 7: GET /usage → shows active session
    let usage_resp = client
        .get(format!("{base_url}/usage"))
        .header("Origin", "http://192.168.1.1:8080")
        .send()
        .await
        .unwrap();
    assert_eq!(usage_resp.status(), 200);
    let usage_text = usage_resp.text().await.unwrap();
    assert_ne!(usage_text, "-1/-1");
    assert!(usage_text.contains('/'));

    stop_server(server).await;
}
