//! HTTP handlers for the Go v1 REST compatibility endpoints.
//!
//! Served alongside the CBOR v2 protocol on the same axum router:
//!   GET  /v1/usage        — JSON {remaining, step_size, metric}
//!   GET  /v1/balance      — JSON {remaining, allotment, step_size}
//!   GET  /v1/whoami       — plain text (node identity)
//!   POST /v1/log-beacon   — diagnostic logging (Issue #69)

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::driver::Driver;
use crate::v1_compat::adapter::{V1Config, get_balance_json};
use crate::v1_compat::merchant::{AdvertConfig, advertisement_json, build_advertisement};

/// Build the v1-compatible REST routes. Returns `Router<Driver>` — the caller
/// applies `.with_state(driver)` once after merging with the v2 router.
pub fn build_router() -> Router<Driver> {
    Router::new()
        .route("/v1/usage", axum::routing::get(handle_usage))
        .route("/v1/balance", axum::routing::get(handle_balance))
        .route("/v1/whoami", axum::routing::get(handle_whoami))
        .route("/v1/advertisement", axum::routing::get(handle_advertisement))
        .route("/v1/log-beacon", axum::routing::post(handle_log_beacon))
}

// ---------------------------------------------------------------------------
// Config snapshot from live driver state
// ---------------------------------------------------------------------------

/// Pull the v1 config snapshot from the driver's live state. `remaining` is the
/// total prepaid balance held across all peers (the `their_balance` sum,
/// clamped to non-negative). `step_size` comes from the advertised pricing's
/// max interval.
async fn v1_config(driver: &Driver) -> V1Config {
    let status = driver.status().await;
    let remaining: u64 = status
        .peers
        .iter()
        .map(|p| p.their_balance.max(0) as u64)
        .sum();
    V1Config::from_pricing(
        status.pricing.min_interval_ms,
        status.pricing.max_interval_ms,
        remaining,
        "milliseconds",
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /v1/usage — Go v1 always returns JSON (Issue #42, Fix 1).
///
/// Returns: `{"remaining": N, "step_size": N, "metric": "milliseconds"}`
async fn handle_usage(State(driver): State<Driver>) -> Response {
    let cfg = v1_config(&driver).await;
    let usage = crate::v1_compat::adapter::UsageJson::from_config(&cfg);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Json(usage),
    )
        .into_response()
}

/// GET /v1/balance — includes the `allotment` field (Issue #42, Fix 2).
///
/// Returns: `{"remaining": N, "allotment": N, "step_size": N}`
async fn handle_balance(State(driver): State<Driver>) -> Response {
    let cfg = v1_config(&driver).await;
    let balance = get_balance_json(&cfg);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Json(balance),
    )
        .into_response()
}

/// GET /v1/whoami — plain text node identity. This format is correct and must
/// NOT be changed (the issue explicitly says so).
async fn handle_whoami(State(driver): State<Driver>) -> Response {
    let status = driver.status().await;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        status.pubkey,
    )
        .into_response()
}

/// GET /v1/advertisement — the mesh advertisement as JSON tag pairs, including
/// both `step_size` and `step` aliases (Issue #42, Fix 3).
pub async fn handle_advertisement(State(driver): State<Driver>) -> Response {
    let cfg = v1_config(&driver).await;
    let advert = AdvertConfig {
        step_size: cfg.step_size,
        metric: cfg.metric.clone(),
    };
    let tags = build_advertisement(&advert);
    let json = advertisement_json(&tags);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Json(json),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /v1/log-beacon (Issue #69)
// ---------------------------------------------------------------------------

/// Request body for the log-beacon endpoint.
#[derive(Debug, Deserialize)]
struct LogBeaconRequest {
    /// Log level: "info", "warn", "error" (case-insensitive, default "info").
    #[serde(default)]
    level: String,
    /// The message to log.
    #[serde(default)]
    message: String,
    /// Optional client identifier.
    #[serde(default)]
    client: String,
}

/// POST /v1/log-beacon — a diagnostic logging endpoint (Issue #69).
///
/// Reads a JSON body `{"level": "info", "message": "...", "client": "..."}`,
/// routes to `tracing` at the appropriate level, and returns
/// `{"logged": true}` with 200 OK.
async fn handle_log_beacon(State(_driver): State<Driver>, body: axum::body::Bytes) -> Response {
    // Parse the JSON body; treat malformed JSON as a 400.
    let req: LogBeaconRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "logged": false,
                    "error": format!("invalid JSON: {e}")
                })),
            )
                .into_response();
        }
    };

    let client = if req.client.is_empty() {
        "unknown"
    } else {
        &req.client
    };
    let message = if req.message.is_empty() {
        "(empty message)"
    } else {
        &req.message
    };

    match req.level.to_lowercase().as_str() {
        "warn" | "warning" => tracing::warn!(client = %client, "beacon: {message}"),
        "error" | "err" => tracing::error!(client = %client, "beacon: {message}"),
        _ => tracing::info!(client = %client, "beacon: {message}"),
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({"logged": true})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::adapter::IpAdapter;
    use crate::config::{Config, Identity};
    use crate::wallet::BootstrapWallet;

    fn test_driver() -> Driver {
        let identity = Arc::new(Identity::load_or_generate(&Config::default()).unwrap());
        Driver::new(
            BootstrapWallet::new(vec![]),
            IpAdapter::new(),
            identity,
            tollgate_core::Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    fn app_with_driver(driver: Driver) -> axum::Router {
        build_router().with_state(driver)
    }

    // --- S1: GET /v1/usage returns JSON ---

    #[tokio::test]
    async fn usage_returns_json_with_remaining_step_size_metric() {
        let app = app_with_driver(test_driver());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("application/json"),
            "usage must be JSON, got content-type={ct}"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("remaining").is_some(), "must have remaining: {json}");
        assert!(json.get("step_size").is_some(), "must have step_size: {json}");
        assert!(json.get("metric").is_some(), "must have metric: {json}");
    }

    // --- S2: GET /v1/balance returns JSON with allotment ---

    #[tokio::test]
    async fn balance_returns_json_with_allotment() {
        let app = app_with_driver(test_driver());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/balance")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("remaining").is_some(), "must have remaining: {json}");
        assert!(
            json.get("allotment").is_some(),
            "must have allotment (Issue #42 Fix 2): {json}"
        );
        assert!(json.get("step_size").is_some(), "must have step_size: {json}");
    }

    // --- S3: whoami returns plain text ---

    #[tokio::test]
    async fn whoami_returns_plain_text() {
        let app = app_with_driver(test_driver());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/whoami")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/plain"), "whoami must be plain text");
    }

    // --- S4: POST /v1/log-beacon logs and returns logged:true ---

    #[tokio::test]
    async fn log_beacon_returns_logged_true() {
        let app = app_with_driver(test_driver());
        let body = serde_json::json!({
            "level": "info",
            "message": "test beacon",
            "client": "test-client"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/log-beacon")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["logged"], serde_json::Value::Bool(true));
    }

    #[tokio::test]
    async fn log_beacon_with_invalid_json_returns_400() {
        let app = app_with_driver(test_driver());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/log-beacon")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn log_beacon_accepts_empty_body() {
        let app = app_with_driver(test_driver());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/log-beacon")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
