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
        .get(format!("http://127.0.0.1:4799/sessions/{mac}"))
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
        .post("http://127.0.0.1:4799/sessions/bootstrap")
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
    assert_eq!(body["active"], serde_json::json!(true));
    assert_eq!(body["session_id"], serde_json::json!(mac));

    let resp = client
        .get(format!("http://127.0.0.1:4799/sessions/{mac}"))
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
        .post(format!("http://127.0.0.1:4799/sessions/{mac}/topups"))
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
}
