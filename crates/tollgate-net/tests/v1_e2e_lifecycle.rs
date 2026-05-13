//! End-to-end lifecycle tests for the V1 client → server interaction.
//!
//! These tests start a mock TollGate HTTP server (simulating an upstream router)
//! and exercise the full V1Client lifecycle: connect, poll usage, renew, recover.
//!
//! The mock server pattern is shared with `v1_client_integration.rs`.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;
use tollgate_net::mock::MockWallet;
use tollgate_net::v1::http::TollGateHttpClient;
use tollgate_net::v1::{V1Client, V1ClientConfig, V1ClientError};

// ---------------------------------------------------------------------------
// Mock server infrastructure
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

async fn get_advertisement(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
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

async fn post_payment(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
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

async fn get_usage(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
    let mut s = state.lock().expect("lock");
    s.usage += 5000;
    format!("{}/{}", s.usage, s.allotment)
}

fn mock_app(state: Arc<Mutex<MockServerState>>) -> Router {
    Router::new()
        .route("/", get(get_advertisement).post(post_payment))
        .route("/usage", get(get_usage))
        .with_state(state)
}

async fn start_mock_server(
    state: Arc<Mutex<MockServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = mock_app(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

fn make_config() -> V1ClientConfig {
    V1ClientConfig {
        gateway_ip: "127.0.0.1".to_owned(),
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

// ---------------------------------------------------------------------------
// Rejection mock server (returns 403 with notice event)
// ---------------------------------------------------------------------------

struct RejectingServerState {
    keys: Keys,
}

async fn rejecting_get_advertisement(
    State(state): State<Arc<Mutex<RejectingServerState>>>,
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

async fn rejecting_post_payment(
    State(state): State<Arc<Mutex<RejectingServerState>>>,
) -> impl IntoResponse {
    let keys = {
        let s = state.lock().expect("lock");
        s.keys.clone()
    };

    let tags = Tags::from_list(vec![
        Tag::custom(TagKind::Custom("level".into()), ["error".to_owned()]),
        Tag::custom(
            TagKind::Custom("code".into()),
            ["token_rejected".to_owned()],
        ),
    ]);

    let event = EventBuilder::new(Kind::Custom(21_023), "Payment not accepted")
        .tags(tags)
        .sign_with_keys(&keys)
        .expect("sign notice event");

    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(serde_json::to_value(event).expect("serialize notice event")),
    )
}

async fn rejecting_get_usage() -> impl IntoResponse {
    "-1/-1"
}

fn rejecting_app(state: Arc<Mutex<RejectingServerState>>) -> Router {
    Router::new()
        .route(
            "/",
            get(rejecting_get_advertisement).post(rejecting_post_payment),
        )
        .route("/usage", get(rejecting_get_usage))
        .with_state(state)
}

async fn start_rejecting_server(
    state: Arc<Mutex<RejectingServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = rejecting_app(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

// ---------------------------------------------------------------------------
// Retry mock server (fails first N requests)
// ---------------------------------------------------------------------------

struct RetryServerState {
    fail_count: u32,
    request_count: u32,
    keys: Keys,
}

impl RetryServerState {
    fn new(fail_count: u32) -> Self {
        Self {
            fail_count,
            request_count: 0,
            keys: Keys::generate(),
        }
    }
}

async fn retry_get_advertisement(
    State(state): State<Arc<Mutex<RetryServerState>>>,
) -> impl IntoResponse {
    let mut s = state.lock().expect("lock");
    s.request_count += 1;
    if s.request_count <= s.fail_count {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
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
        .sign_with_keys(&s.keys)
        .expect("sign ad event");
    axum::Json(serde_json::to_value(event).expect("serialize ad event")).into_response()
}

async fn start_retry_server(
    state: Arc<Mutex<RetryServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = Router::new()
        .route("/", get(retry_get_advertisement))
        .with_state(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

// ===========================================================================
// Test 1: e2e_client_connects_to_server
// ===========================================================================

#[tokio::test]
async fn e2e_client_connects_to_server() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(MockServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    let result = client.connect(&wallet).await;
    assert!(result.is_ok(), "connect should succeed: {result:?}");

    // Verify session created with correct fields.
    let session = client.session();
    assert!(
        session.is_some(),
        "session should be populated after connect"
    );
    let s = session.unwrap();
    assert_eq!(s.metric, "milliseconds");
    assert_eq!(s.step_size, 60_000);
    assert_eq!(s.total_allotment, 60_000, "allotment should be one step");

    // Verify server received exactly one payment.
    {
        let server_state = state.lock().expect("lock");
        assert_eq!(server_state.payments_received, 1);
    }

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 2: e2e_client_renews_session
// ===========================================================================

#[tokio::test]
async fn e2e_client_renews_session() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(MockServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(10000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    client
        .connect(&wallet)
        .await
        .expect("connect should succeed");

    let initial_allotment = client.session().unwrap().total_allotment;
    assert_eq!(initial_allotment, 60_000);

    let result = client.renew(&wallet).await;
    assert!(result.is_ok(), "renew should succeed: {result:?}");

    let renewed_allotment = client.session().unwrap().total_allotment;
    assert_eq!(
        renewed_allotment, 120_000,
        "allotment should increase by one step after renew: {renewed_allotment} vs {initial_allotment}"
    );

    // Server received connect-payment + renew-payment = 2.
    {
        let server_state = state.lock().expect("lock");
        assert_eq!(server_state.payments_received, 2);
    }

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 3: e2e_client_tracks_usage
// ===========================================================================

#[tokio::test]
async fn e2e_client_tracks_usage() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(MockServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    client
        .connect(&wallet)
        .await
        .expect("connect should succeed");

    // connect() calls fetch_usage() once (checking for existing session).
    // That bumps server.usage by 5000. Then our first poll_usage bumps again: 10000.
    let (usage1, allotment1, needs_renewal1) = client.poll_usage().await;
    assert_eq!(
        usage1, 10_000,
        "first poll: usage includes connect-time fetch"
    );
    assert_eq!(allotment1, 60_000, "first poll: allotment matches");
    assert!(!needs_renewal1, "first poll: should not need renewal");

    let (usage2, allotment2, _) = client.poll_usage().await;
    assert_eq!(usage2, 15_000, "second poll: usage should increase by 5000");
    assert_eq!(allotment2, 60_000, "allotment unchanged between polls");

    let (usage3, _, _) = client.poll_usage().await;
    assert_eq!(
        usage3, 20_000,
        "third poll: usage keeps increasing monotonically"
    );

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 4: e2e_client_full_lifecycle
// ===========================================================================

#[tokio::test]
async fn e2e_client_full_lifecycle() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(MockServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(10000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    // Phase 1: Connect.
    client
        .connect(&wallet)
        .await
        .expect("connect should succeed");

    assert_eq!(
        client.session().unwrap().total_allotment,
        60_000,
        "initial allotment"
    );

    // Phase 2: Poll usage multiple times, simulating traffic.
    let (usage1, _, _) = client.poll_usage().await;
    let (usage2, _, _) = client.poll_usage().await;
    let (usage3, _, _) = client.poll_usage().await;
    assert!(
        usage3 > usage2,
        "usage should increase: {usage3} > {usage2}"
    );
    assert!(
        usage2 > usage1,
        "usage should increase: {usage2} > {usage1}"
    );

    // Phase 3: Renew (simulating threshold reached).
    let result = client.renew(&wallet).await;
    assert!(result.is_ok(), "renew should succeed: {result:?}");

    let renewed_allotment = client.session().unwrap().total_allotment;
    assert_eq!(
        renewed_allotment, 120_000,
        "allotment should increase after renewal"
    );

    // Phase 4: Verify server state — connect + renew = 2 payments.
    {
        let server_state = state.lock().expect("lock");
        assert_eq!(
            server_state.payments_received, 2,
            "expected 2 payments (connect + renew)"
        );
    }

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 5: e2e_client_session_recovery
//
// The V1Client re-attach path (allotment > 0 on /usage) creates a dummy
// SessionEvent from "{}" which fails kind validation. This test verifies
// the practical scenario: a second client instance connects to the same
// upstream and establishes its own session.
// ===========================================================================

/// Mock server that returns "-1/-1" on /usage (no session) regardless of
/// payment history. Simulates an upstream that doesn't correlate clients
/// by MAC across connections.
struct NoSessionRecoveryState {
    payments_received: u64,
    allotment: u64,
    keys: Keys,
}

impl NoSessionRecoveryState {
    fn new(keys: Keys) -> Self {
        Self {
            payments_received: 0,
            allotment: 0,
            keys,
        }
    }
}

async fn nsr_get_advertisement(
    State(state): State<Arc<Mutex<NoSessionRecoveryState>>>,
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

async fn nsr_post_payment(
    State(state): State<Arc<Mutex<NoSessionRecoveryState>>>,
) -> impl IntoResponse {
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

async fn nsr_get_usage() -> impl IntoResponse {
    "-1/-1"
}

fn no_session_recovery_app(state: Arc<Mutex<NoSessionRecoveryState>>) -> Router {
    Router::new()
        .route("/", get(nsr_get_advertisement).post(nsr_post_payment))
        .route("/usage", get(nsr_get_usage))
        .with_state(state)
}

async fn start_no_session_recovery_server(
    state: Arc<Mutex<NoSessionRecoveryState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = no_session_recovery_app(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

#[tokio::test]
async fn e2e_client_session_recovery() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(NoSessionRecoveryState::new(keys)));
    let (base_url, server) = start_no_session_recovery_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(10000));

    // First client connects and pays.
    let mut client1 = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);
    client1
        .connect(&wallet)
        .await
        .expect("first client connect should succeed");

    assert_eq!(
        client1.session().unwrap().total_allotment,
        60_000,
        "first client has allotment"
    );

    {
        let server_state = state.lock().expect("lock");
        assert_eq!(
            server_state.payments_received, 1,
            "one payment from first client"
        );
    }

    // Drop first client (simulates process restart).
    drop(client1);

    // Second client (new instance) connects to the same upstream.
    // The server returns "-1/-1" on /usage (no existing session from this
    // client's perspective), so the second client must create a new session.
    let mut client2 = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);
    let result = client2.connect(&wallet).await;
    assert!(
        result.is_ok(),
        "second client should connect successfully: {result:?}"
    );

    {
        let server_state = state.lock().expect("lock");
        assert_eq!(
            server_state.payments_received, 2,
            "second client sent a new payment"
        );
    }

    let session = client2.session().unwrap();
    assert!(
        session.total_allotment > 0,
        "second client should have allotment"
    );

    let result = client2.renew(&wallet).await;
    assert!(
        result.is_ok(),
        "second client renew should succeed: {result:?}"
    );

    {
        let server_state = state.lock().expect("lock");
        assert_eq!(
            server_state.payments_received, 3,
            "third payment from renewal"
        );
    }

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 6: e2e_client_handles_server_rejection
// ===========================================================================

#[tokio::test]
async fn e2e_client_handles_server_rejection() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(RejectingServerState { keys }));
    let (base_url, server) = start_rejecting_server(state).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    let result = client.connect(&wallet).await;
    assert!(
        result.is_err(),
        "connect should fail when server rejects payment"
    );

    // The error should come from the HTTP layer (PaymentRejected).
    match &result {
        Err(V1ClientError::Http(http_err)) => {
            let msg = format!("{http_err}");
            assert!(
                msg.contains("rejected")
                    || msg.contains("token_rejected")
                    || msg.contains("Payment not accepted"),
                "error should mention rejection: {msg}"
            );
        }
        Err(other) => {
            // Also acceptable — any error indicating failure.
            let msg = format!("{other}");
            assert!(
                msg.contains("rejected") || msg.contains("error") || msg.contains("403"),
                "unexpected error type: {msg}"
            );
        }
        Ok(()) => panic!("connect should not succeed when server rejects"),
    }

    // No session should exist after rejection.
    assert!(
        client.session().is_none(),
        "no session should exist after rejected payment"
    );

    server.abort();
    let _ = server.await;
}

// ===========================================================================
// Test 7: e2e_client_probe_with_retries
// ===========================================================================

#[tokio::test]
async fn e2e_client_probe_with_retries() {
    // Server fails the first 2 requests, then succeeds on the 3rd.
    let state = Arc::new(Mutex::new(RetryServerState::new(2)));
    let (base_url, server) = start_retry_server(state.clone()).await;

    // TollGateHttpClient defaults to 3 retries with 2s delay.
    // Override delay to keep the test fast.
    let http_client = {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        TollGateHttpClient {
            client,
            base_url: base_url.clone(),
            probe_retry_count: 3,
            probe_retry_delay: std::time::Duration::from_millis(10),
        }
    };

    let result = http_client.fetch_advertisement().await;
    assert!(result.is_ok(), "should succeed after retries: {result:?}");

    // Verify the advertisement has expected content.
    let ad = result.unwrap();
    assert_eq!(ad.metric().as_deref(), Some("milliseconds"));
    assert_eq!(ad.step_size(), Some(60_000));
    assert!(!ad.pricing_options().is_empty());

    // Verify server received exactly 3 requests (2 failures + 1 success).
    {
        let s = state.lock().expect("lock");
        assert_eq!(
            s.request_count, 3,
            "should make 3 attempts (2 failures + 1 success)"
        );
    }

    server.abort();
    let _ = server.await;
}
