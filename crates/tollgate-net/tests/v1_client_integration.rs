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
