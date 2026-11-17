//! E2E: delegated bootstrap/topup against a live fakewallet mint.
//!
//! Requires: tollgate server running on 127.0.0.1:4799 with
//! TOLLGATE_API_KEY=testkey123, testnut.cashu.space as the configured
//! mint, and metric=milliseconds step_size=60000 price_per_step=1:
//!
//! ```yaml
//! listen: 127.0.0.1:4799
//! mints: [https://testnut.cashu.space]
//! v1_compat:
//!   metric: milliseconds
//!   step_size: 60000
//!   accepted_mints:
//!     - {url: https://testnut.cashu.space, price_per_step: 1, unit: sat, min_steps: 1}
//!   wallet_seed: "<128 hex chars>"
//! ```
//!
//! Run: cargo test --bin tollgate --features v1-compat e2e_testnut -- --ignored --nocapture

fn token_value_sat(token: &str) -> u64 {
    let parsed: cashu::nuts::Token = token.parse().expect("token parses");
    let amount = parsed.value().expect("token value");
    amount.into()
}

#[tokio::test]
#[ignore = "needs live testnut mint + running tollgate server"]
async fn e2e_bootstrap_and_topup_with_real_tokens() {
    use std::io::Read;
    // Random per-run seed: cdk derives deterministic secrets (NUT-13) from
    // the seed + a localstore counter — reusing a seed with a fresh store
    // replays blinded messages the mint has already signed.
    let mut seed = [0u8; 64];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .expect("random seed");

    let wallet = crate::v1_compat::wallet::CdkWallet::new("https://testnut.cashu.space", seed)
        .await
        .expect("client wallet");

    // testnut charges ~10% send fee (100ppk): mint well above the send
    // amount so the fee comes out of change, not the sent proofs.
    wallet.mint_test_tokens(1000).await.expect("mint test sats");
    let token = String::from_utf8(
        wallet
            .create_token(21, "https://testnut.cashu.space")
            .await
            .expect("create token"),
    )
    .expect("utf8 token");
    println!("token[0..40]: {}", &token[..token.len().min(40)]);
    let v1 = token_value_sat(&token);

    let client = reqwest::Client::new();
    let mac = "02:00:00:00:00:01";

    // Bootstrap adds to any pre-existing session for the MAC
    // (add_allotment semantics), so expectations derive from the
    // observed baseline rather than assuming a fresh store.
    let resp = client
        .get(format!("http://127.0.0.1:4799/v1/sessions/{mac}"))
        .header("x-api-key", "testkey123")
        .send()
        .await
        .expect("baseline get");
    let baseline: u64 = if resp.status() == reqwest::StatusCode::OK {
        resp.json::<serde_json::Value>()
            .await
            .expect("baseline body")["allotment"]
            .as_u64()
            .expect("baseline allotment")
    } else {
        0
    };

    let resp = client
        .post("http://127.0.0.1:4799/v1/sessions/bootstrap")
        .header("x-api-key", "testkey123")
        .json(&serde_json::json!({"access_method": "e2e", "token": token, "mac": mac}))
        .send()
        .await
        .expect("bootstrap request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("bootstrap body");
    println!("bootstrap -> {status} {body}");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "bootstrap must succeed: {body}"
    );
    assert_eq!(body["allotment"], serde_json::json!(baseline + v1 * 60_000));
    assert_eq!(body["metric"], "milliseconds");
    assert_eq!(body["access_level"], "active");
    assert_eq!(body["session_id"], serde_json::json!(mac));

    let resp = client
        .get(format!("http://127.0.0.1:4799/v1/sessions/{mac}"))
        .header("x-api-key", "testkey123")
        .send()
        .await
        .expect("get session");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("session body");
    assert_eq!(body["allotment"], serde_json::json!(baseline + v1 * 60_000));

    let token2 = String::from_utf8(
        wallet
            .create_token(10, "https://testnut.cashu.space")
            .await
            .expect("create token 2"),
    )
    .expect("utf8 token");
    let v2 = token_value_sat(&token2);
    let resp = client
        .post(format!("http://127.0.0.1:4799/v1/sessions/{mac}/topups"))
        .header("x-api-key", "testkey123")
        .json(&serde_json::json!({"token": token2}))
        .send()
        .await
        .expect("topup request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("topup body");
    println!("topup -> {status} {body}");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "topup must succeed: {body}"
    );
    assert_eq!(
        body["allotment"],
        serde_json::json!(baseline + (v1 + v2) * 60_000)
    );

    // Phase-2 delegated Bootstrap (tollgate-auth Go client shape): raw
    // token body + X-TollGate-MAC → Nostr kind 1022 session event.
    let token3 = String::from_utf8(
        wallet
            .create_token(5, "https://testnut.cashu.space")
            .await
            .expect("create token 3"),
    )
    .expect("utf8 token");
    let resp = client
        .post("http://127.0.0.1:4799/")
        .header("x-tollgate-mac", "02:00:00:00:00:02")
        .header("content-type", "text/plain")
        .body(token3)
        .send()
        .await
        .expect("phase-2 bootstrap");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("phase-2 body");
    println!("phase2 -> {status} {body}");
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["kind"], serde_json::json!(1022));

    // The delegated consumer's GetSession must see portal-bootstrapped
    // sessions (store credit on header-identified payments).
    let resp = client
        .get("http://127.0.0.1:4799/v1/sessions/02:00:00:00:00:02")
        .header("x-api-key", "testkey123")
        .send()
        .await
        .expect("phase-2 get");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// Live check for the /usage unit mismatch (2026-11-17 RADIUS E2E
/// finding): a delegated (X-TollGate-MAC) caller with a store session
/// must read METRIC units "consumed/allotment" — Go v1 GetUsage shape —
/// never the driver's msat balance.
///
/// Fully in-process: the v1 router is built with an in-memory wallet
/// repo and session store and served on 127.0.0.1:4809; the client
/// mints ONE real testnut.cashu.space token and pays over real HTTP.
/// The only shared state touched is the pending-receive journal
/// (/var/lib/tollgate/pending-receives.jsonl), entry in-and-out on the
/// happy path.
///
/// Run: cargo test --bin tollgate --features v1-compat e2e_usage_units -- --ignored --nocapture
#[tokio::test]
#[ignore = "mints a real testnut.cashu.space token"]
async fn e2e_usage_units_match_the_store_session() {
    use std::io::Read;
    use std::str::FromStr;
    use std::sync::Arc;
    const MINT: &str = "https://testnut.cashu.space";
    const MAC: &str = "02:e2:75:00:00:01";

    let mut server_seed = [0u8; 64];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut server_seed))
        .expect("server seed");
    // In-memory localstore: redemption state must not touch the shared
    // /var/lib/tollgate/cdk-wallet.sqlite.
    let localstore = cdk_sqlite::wallet::memory::empty()
        .await
        .expect("in-memory localstore");
    let repo = Arc::new(
        cdk::wallet::WalletRepositoryBuilder::new()
            .localstore(Arc::new(localstore))
            .seed(server_seed)
            .build()
            .await
            .expect("server wallet repo"),
    );
    repo.add_wallet(cdk::mint_url::MintUrl::from_str(MINT).expect("mint url"))
        .await
        .expect("server mint wallet");
    let bootstrap =
        crate::wallet::BootstrapWallet::new(vec![MINT.to_owned()]).with_wallet_repo(repo);

    let identity = Arc::new(
        crate::config::Identity::load_or_generate(&crate::config::Config::default()).unwrap(),
    );
    let driver = crate::driver::Driver::new(
        bootstrap,
        crate::adapter::IpAdapter::new(),
        identity,
        tollgate_core::Price::default(),
        "milliseconds",
        Vec::new(),
    );

    let v1_cfg = crate::config::V1CompatConfig {
        metric: "milliseconds".to_owned(),
        step_size: 60_000,
        accepted_mints: vec![crate::config::V1AcceptedMint {
            url: MINT.to_owned(),
            price_per_step: 1,
            unit: "sat".to_owned(),
            min_steps: 1,
        }],
        ..Default::default()
    };
    let config = crate::server::build_v1_config(&v1_cfg, None);
    let sessions = Arc::new(crate::v1_compat::session_store::InMemorySessionStore::new());
    let app = crate::v1_compat::handlers::build_router(driver, config, Some(sessions));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:4809")
        .await
        .expect("bind 127.0.0.1:4809");
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });

    let mut client_seed = [0u8; 64];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut client_seed))
        .expect("client seed");
    let wallet = crate::v1_compat::wallet::CdkWallet::new(MINT, client_seed)
        .await
        .expect("client wallet");
    // One mint op funds the run; create_token spends from balance.
    wallet.mint_test_tokens(1000).await.expect("mint test sats");
    let token = String::from_utf8(wallet.create_token(4, MINT).await.expect("create token"))
        .expect("utf8 token");

    let http = reqwest::Client::new();
    let resp = http
        .post("http://127.0.0.1:4809/")
        .header("x-tollgate-mac", MAC)
        .header("content-type", "text/plain")
        .body(token)
        .send()
        .await
        .expect("delegated payment");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("payment body");
    println!("payment -> {status} {body}");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "payment must succeed: {body}"
    );
    assert_eq!(body["kind"], serde_json::json!(1022));
    // testnut fee granularity means the paid value may exceed the
    // requested sats (a 4-sat create_token yielded 5): derive the
    // expected allotment from the 1022 event itself.
    let expected_allotment: i64 = body["tags"]
        .as_array()
        .expect("1022 tags")
        .iter()
        .find(|t| t[0] == "allotment")
        .expect("allotment tag")[1]
        .as_str()
        .expect("allotment value")
        .parse()
        .expect("allotment ms");

    let resp = http
        .get("http://127.0.0.1:4809/usage")
        .header("x-tollgate-mac", MAC)
        .send()
        .await
        .expect("usage poll");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let text = resp.text().await.expect("usage body");
    println!("usage -> {text}");
    let (consumed, allotment) = text
        .split_once('/')
        .unwrap_or_else(|| panic!("usage must be 'consumed/allotment', got {text}"));
    let consumed: i64 = consumed.parse().expect("consumed ms");
    let allotment: i64 = allotment.parse().expect("allotment ms");
    // Metric ms from the store session (e.g. 300000 for a 5-sat
    // token at 60000 ms/sat), NOT the msat driver balance (5000).
    assert_eq!(
        allotment, expected_allotment,
        "allotment must be metric ms, got {text}"
    );
    assert!(
        (0..10_000).contains(&consumed),
        "consumed must be small ms-scale elapsed, got {text}"
    );
    server.abort();
}
