use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use nostr::prelude::*;
use reqwest::Client;
use tokio::sync::Mutex;
use tollgate_net::mock::MockWallet;
use tollgate_net::v1::server::handlers::build_router;
use tollgate_net::v1::server::{
    build_advertisement, AcceptedMint, ServerState, StubMacResolver, StubValve, V1ServerConfig,
};

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

fn mock_token(amount_sats: u64) -> Vec<u8> {
    amount_sats.to_be_bytes().to_vec()
}

async fn start_server(
    config: V1ServerConfig,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<ServerState<MockWallet>>,
) {
    let wallet = Arc::new(MockWallet::new(0));
    let advertisement = build_advertisement(&config).unwrap();

    let state = Arc::new(ServerState {
        wallet: wallet.clone(),
        config,
        sessions: Mutex::new(HashMap::new()),
        mac_resolver: Arc::new(StubMacResolver::default()),
        valve: Arc::new(StubValve),
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

#[tokio::test]
async fn v1_server_returns_advertisement() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.get(&base_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(10_021));

    let has_pricing = event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(String::as_str) == Some("price_per_step"));
    assert!(has_pricing);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_accepts_payment() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    let resp = client.post(&base_url).body(token).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(1022));

    let allotment = event.tags.iter().find_map(|tag| {
        let items = tag.as_slice();
        if items.first().map(String::as_str) == Some("allotment") {
            items.get(1).and_then(|s| s.parse::<u64>().ok())
        } else {
            None
        }
    });
    assert_eq!(allotment, Some(600_000)); // 10 steps * 60000

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_tracks_usage() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    client.post(&base_url).body(token).send().await.unwrap();

    let resp = client
        .get(format!("{base_url}/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let parts: Vec<&str> = body.trim().split('/').collect();
    assert_eq!(parts.len(), 2);

    let usage: i64 = parts[0].parse().unwrap();
    let allotment: i64 = parts[1].parse().unwrap();
    assert!(usage >= 0);
    assert_eq!(allotment, 600_000);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_whoami() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/whoami"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert!(body.starts_with("mac="));
    assert!(body.contains("00:11:22:33:44:55"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_balance() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let token = mock_token(10);
    client.post(&base_url).body(token).send().await.unwrap();

    let resp = client
        .get(format!("{base_url}/balance"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], true);
    assert_eq!(json["metric"], "milliseconds");
    assert_eq!(json["allotment"], 600_000);
    assert!(json["remaining"].as_u64().unwrap() > 0);
    assert!(json["start_time"].as_i64().unwrap() > 0);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_rejects_invalid_token() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client.post(&base_url).body("garbage").send().await.unwrap();
    assert_eq!(resp.status(), 400);

    let body = resp.text().await.unwrap();
    let event: Event = Event::from_json(&body).unwrap();
    assert_eq!(event.kind, Kind::Custom(21_023));

    let has_error_code = event.tags.iter().any(|tag| {
        let items = tag.as_slice();
        items.first().map(String::as_str) == Some("code")
    });
    assert!(has_error_code);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_balance_no_session() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/balance"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], 1);
    assert_eq!(json["session_active"], false);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_server_usage_no_session() {
    let (base_url, server, _state) = start_server(test_config()).await;
    let client = Client::new();

    let resp = client
        .get(format!("{base_url}/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert_eq!(body.trim(), "-1/-1");

    server.abort();
    let _ = server.await;
}
