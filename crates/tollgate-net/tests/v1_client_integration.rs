use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;
use tollgate_net::mock::MockWallet;
use tollgate_net::v1::{V1Client, V1ClientConfig, V1ClientError};

struct ServerState {
    payments_received: u64,
    usage: u64,
    allotment: u64,
    keys: Keys,
}

impl ServerState {
    fn new(keys: Keys) -> Self {
        Self {
            payments_received: 0,
            usage: 0,
            allotment: 0,
            keys,
        }
    }
}

async fn get_advertisement(State(state): State<Arc<Mutex<ServerState>>>) -> impl IntoResponse {
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

async fn post_payment(State(state): State<Arc<Mutex<ServerState>>>) -> impl IntoResponse {
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

async fn get_usage(State(state): State<Arc<Mutex<ServerState>>>) -> impl IntoResponse {
    let mut s = state.lock().expect("lock");
    s.usage += 5000;
    format!("{}/{}", s.usage, s.allotment)
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
        recovery_file: std::path::PathBuf::from("/tmp/tollgate-test-recovery.txt"),
    }
}

fn mock_app(state: Arc<Mutex<ServerState>>) -> Router {
    Router::new()
        .route("/", get(get_advertisement).post(post_payment))
        .route("/usage", get(get_usage))
        .with_state(state)
}

async fn start_mock_server(
    state: Arc<Mutex<ServerState>>,
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

fn incompatible_mint_app(state: Arc<Mutex<ServerState>>) -> Router {
    Router::new()
        .route(
            "/",
            get(|State(state): State<Arc<Mutex<ServerState>>>| async move {
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
                            "https://mint.example.com".to_owned(),
                            "1".to_owned(),
                        ],
                    ),
                ]);
                let event = EventBuilder::new(Kind::Custom(10_021), "")
                    .tags(tags)
                    .sign_with_keys(&keys)
                    .expect("sign ad event");
                axum::Json(serde_json::to_value(event).expect("serialize ad event"))
            }),
        )
        .route("/usage", get(|| async { "0/0" }))
        .with_state(state)
}

async fn start_incompatible_server(
    state: Arc<Mutex<ServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let app = incompatible_mint_app(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

#[tokio::test]
async fn v1_client_connect_establishes_session() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    let result = client.connect(&wallet).await;
    assert!(result.is_ok(), "connect should succeed: {result:?}");

    let session = client.session();
    assert!(
        session.is_some(),
        "session should be populated after connect"
    );
    let s = session.unwrap();
    assert_eq!(s.metric, "milliseconds");
    assert_eq!(s.step_size, 60_000);
    assert!(s.total_allotment > 0, "allotment should be positive");

    {
        let server_state = state.lock().expect("lock");
        assert_eq!(server_state.payments_received, 1);
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_poll_usage_tracks_metrics() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    client
        .connect(&wallet)
        .await
        .expect("connect should succeed");

    let (usage, allotment, needs_renewal) = client.poll_usage().await;
    // connect() calls fetch_usage() once (checking for existing session), then
    // this poll increments again: 5000 + 5000 = 10000.
    assert_eq!(usage, 10_000);
    assert!(allotment > 0);
    assert!(!needs_renewal, "should not need renewal yet");

    let (usage2, _, _) = client.poll_usage().await;
    assert_eq!(usage2, 15_000, "usage should keep increasing");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_renew_extends_session() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
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
    assert!(
        renewed_allotment > initial_allotment,
        "allotment should increase after renew: {renewed_allotment} vs {initial_allotment}"
    );

    {
        let server_state = state.lock().expect("lock");
        assert_eq!(server_state.payments_received, 2);
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_rejects_incompatible_mint() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_incompatible_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    let result = client.connect(&wallet).await;
    assert!(
        matches!(result, Err(V1ClientError::Pricing(_))),
        "should get PricingError for incompatible mint, got: {result:?}"
    );

    assert!(
        client.session().is_none(),
        "no session should exist after failed connect"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_renew_throttles_rapid_calls() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_mock_server(state.clone()).await;

    let wallet = Arc::new(MockWallet::new(10000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    client
        .connect(&wallet)
        .await
        .expect("connect should succeed");

    let result1 = client.renew(&wallet).await;
    assert!(result1.is_ok(), "first renew should succeed: {result1:?}");

    let result2 = client.renew(&wallet).await;
    assert!(result2.is_ok(), "throttled renew should return Ok (no-op)");

    {
        let server_state = state.lock().expect("lock");
        // connect sends 1 payment, first renew sends 1 payment, second renew is throttled (0)
        assert_eq!(
            server_state.payments_received, 2,
            "should only have 2 payments (connect + first renew), not 3"
        );
    }

    server.abort();
    let _ = server.await;
}

// ── Payment-failure recovery tests ──────────────────────────────────

use std::future::Future;
use std::pin::Pin;

use axum::http::StatusCode;
use tollgate_core::error::WalletError;
use tollgate_core::types::Amount;
use tollgate_core::wallet::Wallet;

async fn reject_payment() -> impl IntoResponse {
    (StatusCode::BAD_GATEWAY, "upstream rejected payment")
}

async fn zero_usage() -> impl IntoResponse {
    // No existing session: allotment 0 forces the fresh-connection path.
    "0/0".to_owned()
}

async fn start_failing_payment_server(
    state: Arc<Mutex<ServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/", get(get_advertisement).post(reject_payment))
        .route("/usage", get(zero_usage))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

struct RejectReceiveWallet {
    inner: MockWallet,
}

impl Wallet for RejectReceiveWallet {
    fn receive_token(
        &self,
        _token: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
        Box::pin(async {
            Err(WalletError::TokenRejected(
                "auto-recovery disabled in test".to_owned(),
            ))
        })
    }

    fn create_token(
        &self,
        amount: Amount,
        mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WalletError>> + Send + '_>> {
        self.inner.create_token(amount, mint_url)
    }

    fn mint_reachable(
        &self,
        mint: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, WalletError>> + Send + '_>> {
        self.inner.mint_reachable(mint)
    }

    fn balance(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
        self.inner.balance()
    }
}

#[tokio::test]
async fn v1_client_reclaims_token_when_payment_submission_fails() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_failing_payment_server(state).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut config = make_config();
    config.recovery_file =
        std::env::temp_dir().join(format!("tollgate-recover-auto-{}.txt", std::process::id()));
    let mut client = V1Client::<MockWallet>::new_with_base_url(config, &base_url);

    let result = client.connect(&wallet).await;
    assert!(
        result.is_err(),
        "connect must fail when the gateway rejects the payment"
    );

    let balance = wallet.balance().await.expect("balance readable").0;
    assert_eq!(
        balance, 1000,
        "payment amount must be auto-reclaimed after a failed submission"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_persists_unrecoverable_token_to_recovery_file() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_failing_payment_server(state).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let recovery_file = dir.path().join("tokens-to-recover.txt");

    let wallet = Arc::new(RejectReceiveWallet {
        inner: MockWallet::new(1000),
    });
    let mut config = make_config();
    config.recovery_file = recovery_file.clone();
    let mut client = V1Client::<RejectReceiveWallet>::new_with_base_url(config, &base_url);

    let result = client.connect(&wallet).await;
    assert!(
        result.is_err(),
        "connect must fail when the gateway rejects the payment"
    );

    let contents = std::fs::read_to_string(&recovery_file)
        .expect("recovery file must exist when auto-reclaim fails");
    assert!(
        contents.contains("mint=https://testnut.cashu.exchange"),
        "recovery entry must name the mint: {contents}"
    );
    assert!(
        contents.contains("token="),
        "recovery entry must carry the token for manual reclaim: {contents}"
    );

    server.abort();
    let _ = server.await;
}

// ── Reattach budget validation ──────────────────────────────────────

async fn existing_session_usage() -> impl IntoResponse {
    "1000/60000".to_owned()
}

async fn start_existing_session_server(
    state: Arc<Mutex<ServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/", get(get_advertisement).post(post_payment))
        .route("/usage", get(existing_session_usage))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{addr}");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock server error");
    });
    (base_url, handle)
}

#[tokio::test]
async fn v1_client_reattach_rejects_pricing_above_budget_cap() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_existing_session_server(state).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut config = make_config();
    // The mock ad prices 1 sat per 60_000 ms step ≈ 1.67e-5 sat/ms — cap
    // below that so the reattached pricing violates the budget.
    config.max_price_per_ms = 1e-6;
    let mut client = V1Client::<MockWallet>::new_with_base_url(config, &base_url);

    let result = client.connect(&wallet).await;
    assert!(
        result.is_err(),
        "reattaching to over-cap pricing must fail, not adopt it silently: {result:?}"
    );
    assert!(
        client.session().is_none(),
        "no session may be adopted from a rejected reattach"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn v1_client_reattach_accepts_pricing_within_budget() {
    let keys = Keys::generate();
    let state = Arc::new(Mutex::new(ServerState::new(keys)));
    let (base_url, server) = start_existing_session_server(state).await;

    let wallet = Arc::new(MockWallet::new(1000));
    let mut client = V1Client::<MockWallet>::new_with_base_url(make_config(), &base_url);

    client
        .connect(&wallet)
        .await
        .expect("reattach within budget must succeed");

    let session = client.session().expect("session adopted");
    assert_eq!(session.total_allotment, 60_000);

    server.abort();
    let _ = server.await;
}
