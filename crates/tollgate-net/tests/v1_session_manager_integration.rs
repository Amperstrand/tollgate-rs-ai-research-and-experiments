//! Integration tests for the V1 SessionManager with real mock servers.
//!
//! These tests exercise multi-gateway session management: creating sessions,
//! disconnecting interfaces, and stopping all sessions. Each test starts its
//! own mock HTTP servers simulating upstream TollGate routers.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;
use tokio_util::sync::CancellationToken;
use tollgate_net::mock::MockWallet;
use tollgate_net::v1::http::TollGateHttpClient;
use tollgate_net::v1::session_manager::{
    SessionManager, SessionManagerConfig, UpstreamSessionState,
};
use tollgate_net::v1::usage_tracker::UsageTrackerConfig;
use tollgate_net::v1::{V1Client, V1ClientConfig};

// ---------------------------------------------------------------------------
// Mock server infrastructure (same pattern as v1_client_integration.rs)
// ---------------------------------------------------------------------------

struct MockServerState {
    payments_received: u64,
    usage: u64,
    allotment: u64,
    keys: Keys,
}

impl MockServerState {
    fn new(keys: Keys) -> Self {
        Self {
            payments_received: 0,
            usage: 0,
            allotment: 0,
            keys,
        }
    }
}

async fn sm_get_advertisement(
    State(state): State<Arc<Mutex<MockServerState>>>,
) -> impl IntoResponse {
    let keys = {
        let s = state.lock().expect("lock");
        s.keys.clone()
    };

    let tags = Tags::from_list(vec![
        Tag::custom(
            TagKind::Custom("metric".into()),
            ["milliseconds".to_owned()],
        ),
        Tag::custom(TagKind::Custom("step_size".into()), ["60000".to_owned()]),
        Tag::custom(
            TagKind::Custom("price_per_step".into()),
            [
                "cashu".to_owned(),
                "1".to_owned(),
                "sat".to_owned(),
                "https://testnut.cashu.exchange".to_owned(),
                "1".to_owned(),
            ],
        ),
    ]);

    let event = EventBuilder::new(Kind::Custom(10_021), "")
        .tags(tags)
        .sign_with_keys(&keys)
        .expect("sign ad event");

    axum::Json(serde_json::to_value(event).expect("serialize ad event"))
}

async fn sm_post_payment(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
    let mut s = state.lock().expect("lock");
    s.payments_received += 1;
    s.allotment += 60_000;

    let tags = Tags::from_list(vec![
        Tag::custom(
            TagKind::Custom("allotment".into()),
            [s.allotment.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("metric".into()),
            ["milliseconds".to_owned()],
        ),
    ]);

    let event = EventBuilder::new(Kind::Custom(1022), "")
        .tags(tags)
        .sign_with_keys(&s.keys)
        .expect("sign session event");

    axum::Json(serde_json::to_value(event).expect("serialize session event"))
}

async fn sm_get_usage(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
    let mut s = state.lock().expect("lock");
    s.usage += 5000;
    format!("{}/{}", s.usage, s.allotment)
}

fn sm_mock_app(state: Arc<Mutex<MockServerState>>) -> Router {
    Router::new()
        .route("/", get(sm_get_advertisement).post(sm_post_payment))
        .route("/usage", get(sm_get_usage))
        .with_state(state)
}

async fn start_sm_mock_server(
    state: Arc<Mutex<MockServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = sm_mock_app(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_client_config(gateway_ip: &str) -> V1ClientConfig {
    V1ClientConfig {
        gateway_ip: gateway_ip.to_owned(),
        mac_address: "00:11:22:33:44:55".to_owned(),
        our_mint_urls: vec!["https://testnut.cashu.exchange".to_owned()],
        unit: "sat".to_owned(),
        max_price_per_ms: 1.0,
        max_price_per_byte: 1.0,
        preferred_allotment: 60_000,
        poll_interval_secs: 1,
        renewal_threshold: 0.8,
    }
}

fn make_manager_config() -> SessionManagerConfig {
    SessionManagerConfig {
        client_config: make_client_config("0.0.0.0"),
        tracker_config: UsageTrackerConfig {
            poll_interval: std::time::Duration::from_millis(50),
            renewal_threshold: 0.8,
        },
    }
}

/// Manually insert a session into the manager's map (simulates what
/// `handle_gateway_connected` does, but with a test URL override).
async fn insert_session(
    manager: &SessionManager<MockWallet>,
    interface_name: &str,
    gateway_ip: &str,
    base_url: &str,
) {
    let mut client =
        V1Client::<MockWallet>::new_with_base_url(make_client_config(gateway_ip), base_url);
    client
        .connect(&manager.wallet)
        .await
        .expect("connect should succeed");

    let cancel = CancellationToken::new();
    let http = TollGateHttpClient::new_with_base_url(base_url);

    let tracker_handle = tollgate_net::v1::usage_tracker::spawn_usage_tracker(
        http,
        manager.config.tracker_config.clone(),
        manager.wallet.clone(),
        manager.renewal_tx.clone(),
        gateway_ip.to_owned(),
        cancel.clone(),
    );

    let session_state = UpstreamSessionState {
        gateway_ip: gateway_ip.to_owned(),
        interface_name: interface_name.to_owned(),
        client,
        tracker_handle: Some(tracker_handle),
        cancel,
        created_at: std::time::Instant::now(),
        last_payment_at: Some(std::time::Instant::now()),
        total_spent_sats: 0,
        payment_count: 1,
    };

    let mut sessions = manager.sessions.write().await;
    sessions.insert(gateway_ip.to_owned(), session_state);
}

// ===========================================================================
// Test 1: sm_manages_two_gateways
// ===========================================================================

#[tokio::test]
async fn sm_manages_two_gateways() {
    let wallet = Arc::new(MockWallet::new(10000));
    let config = make_manager_config();
    let manager = SessionManager::new(config, wallet);

    let mut server_handles = Vec::new();

    // Start two mock servers on different ports, simulating two upstream gateways.
    let keys1 = Keys::generate();
    let state1 = Arc::new(Mutex::new(MockServerState::new(keys1)));
    let (base_url1, server1) = start_sm_mock_server(state1).await;
    server_handles.push(server1);

    let keys2 = Keys::generate();
    let state2 = Arc::new(Mutex::new(MockServerState::new(keys2)));
    let (base_url2, server2) = start_sm_mock_server(state2).await;
    server_handles.push(server2);

    insert_session(&manager, "eth0", "10.0.0.1", &base_url1).await;
    insert_session(&manager, "wlan0", "10.0.0.2", &base_url2).await;

    let active = manager.get_active_sessions().await;
    assert_eq!(active.len(), 2, "should have two active sessions");

    let gateway_ips: Vec<&str> = active.iter().map(|s| s.gateway_ip.as_str()).collect();
    assert!(gateway_ips.contains(&"10.0.0.1"), "should contain 10.0.0.1");
    assert!(gateway_ips.contains(&"10.0.0.2"), "should contain 10.0.0.2");

    for s in &active {
        assert!(
            s.total_allotment > 0,
            "session for {} should have allotment",
            s.gateway_ip
        );
        assert_eq!(s.metric, "milliseconds");
    }

    manager.stop().await;
    for s in server_handles {
        s.abort();
        let _ = s.await;
    }
}

// ===========================================================================
// Test 2: sm_disconnect_removes_sessions
// ===========================================================================

#[tokio::test]
async fn sm_disconnect_removes_sessions() {
    let wallet = Arc::new(MockWallet::new(10000));
    let config = make_manager_config();
    let manager = SessionManager::new(config, wallet);

    let mut server_handles = Vec::new();

    let keys1 = Keys::generate();
    let state1 = Arc::new(Mutex::new(MockServerState::new(keys1)));
    let (base_url1, server1) = start_sm_mock_server(state1).await;
    server_handles.push(server1);

    let keys2 = Keys::generate();
    let state2 = Arc::new(Mutex::new(MockServerState::new(keys2)));
    let (base_url2, server2) = start_sm_mock_server(state2).await;
    server_handles.push(server2);

    insert_session(&manager, "eth0", "10.0.0.1", &base_url1).await;
    insert_session(&manager, "wlan0", "10.0.0.2", &base_url2).await;

    assert_eq!(
        manager.get_active_sessions().await.len(),
        2,
        "should start with two sessions"
    );

    // Disconnect eth0 — only the 10.0.0.1 session should be removed.
    manager
        .handle_disconnect("eth0")
        .await
        .expect("disconnect should succeed");

    let active = manager.get_active_sessions().await;
    assert_eq!(active.len(), 1, "only one session should remain");
    assert_eq!(active[0].gateway_ip, "10.0.0.2");
    assert_eq!(active[0].interface_name, "wlan0");

    manager.stop().await;
    for s in server_handles {
        s.abort();
        let _ = s.await;
    }
}

// ===========================================================================
// Test 3: sm_stop_cleans_up
// ===========================================================================

#[tokio::test]
async fn sm_stop_cleans_up() {
    let wallet = Arc::new(MockWallet::new(10000));
    let config = make_manager_config();
    let manager = SessionManager::new(config, wallet);

    let mut server_handles = Vec::new();

    let keys1 = Keys::generate();
    let state1 = Arc::new(Mutex::new(MockServerState::new(keys1)));
    let (base_url1, server1) = start_sm_mock_server(state1).await;
    server_handles.push(server1);

    insert_session(&manager, "eth0", "10.0.0.1", &base_url1).await;

    assert_eq!(
        manager.get_active_sessions().await.len(),
        1,
        "should have one session before stop"
    );

    manager.stop().await;

    assert_eq!(
        manager.get_active_sessions().await.len(),
        0,
        "all sessions should be removed after stop"
    );

    for s in server_handles {
        s.abort();
        let _ = s.await;
    }
}
