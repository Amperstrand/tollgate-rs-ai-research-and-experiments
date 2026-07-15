use std::sync::Arc;

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;

use crate::driver::Driver;
use crate::v1_compat::adapter;
use crate::v1_compat::merchant::{self, V1ServerConfig};

pub fn build_router(driver: Driver, config: Arc<V1ServerConfig>) -> Router {
    Router::new()
        .route(
            "/",
            get(handle_get_details).post(handle_post_payment),
        )
        .route("/pay", get(handle_get_details))
        .route("/usage", get(handle_usage))
        .route("/whoami", get(handle_whoami))
        .route("/balance", get(handle_balance))
        .route("/ln-invoice", get(handle_get_ln_invoice).post(handle_post_ln_invoice))
        .layer(axum::Extension(config))
        .with_state(driver)
}

fn extract_payment_token(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.starts_with("cashu") || trimmed.starts_with("cashuA") || trimmed.starts_with("cashuB") {
        return Some(trimmed.to_owned());
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(token) = json.get("token").and_then(|t| t.as_str()) {
            return Some(token.to_owned());
        }
    }
    None
}

async fn handle_get_details(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
) -> Response {
    match merchant::build_advertisement(&config) {
        Ok(json) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            json,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to build advertisement: {e}"),
        )
            .into_response(),
    }
}

async fn handle_post_payment(
    State(driver): State<Driver>,
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let token = match extract_payment_token(&body) {
        Some(t) => t,
        None => {
            return (StatusCode::BAD_REQUEST, "missing or invalid token").into_response();
        }
    };

    let client_ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
        });

    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(hex) => hex,
        None => {
            return (StatusCode::BAD_REQUEST, "could not resolve client identity").into_response();
        }
    };

    let result = adapter::admit_peer_with_token(&driver, &peer_hex, client_ip, &token).await;

    if result.accepted {
        let session = merchant::CustomerSession {
            mac_address: peer_hex.clone(),
            start_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            metric: config.metric.clone(),
            allotment: 0,
        };
        match merchant::build_session_event(
            &session,
            &config,
            &peer_hex,
            0,
            "cashu",
        ) {
            Ok(event_json) => (
                StatusCode::OK,
                [("content-type", "application/json")],
                event_json,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("session event error: {e}"),
            )
                .into_response(),
        }
    } else {
        let notice = merchant::build_notice_event(
            "error",
            "payment_rejected",
            result.reason.as_deref().unwrap_or("rejected"),
            Some(&peer_hex),
            &config,
        );
        match notice {
            Ok(json) => (StatusCode::PAYMENT_REQUIRED, json).into_response(),
            Err(_) => (StatusCode::PAYMENT_REQUIRED, "payment rejected").into_response(),
        }
    }
}

async fn handle_usage(
    State(driver): State<Driver>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(&headers, Some(&ConnectInfo(addr)));
    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(h) => h,
        None => return (StatusCode::BAD_REQUEST, "unknown client").into_response(),
    };
    match adapter::get_usage_text(&driver, &peer_hex).await {
        Some(text) => (StatusCode::OK, text).into_response(),
        None => (StatusCode::NOT_FOUND, "no active session").into_response(),
    }
}

async fn handle_whoami(
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(&headers, Some(&ConnectInfo(addr)));
    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(h) => h,
        None => return (StatusCode::BAD_REQUEST, "unknown").into_response(),
    };
    let mac_hex = &peer_hex[2..14];
    let mac = format!(
        "{}:{}:{}:{}:{}:{}",
        &mac_hex[0..2],
        &mac_hex[2..4],
        &mac_hex[4..6],
        &mac_hex[6..8],
        &mac_hex[8..10],
        &mac_hex[10..12],
    );
    (StatusCode::OK, format!("mac={mac}")).into_response()
}

async fn handle_balance(
    State(driver): State<Driver>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(&headers, Some(&ConnectInfo(addr)));
    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(h) => h,
        None => return (StatusCode::BAD_REQUEST, "unknown client").into_response(),
    };
    match adapter::get_balance_json(&driver, &peer_hex).await {
        Some(json) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::to_string(&json).unwrap_or_default(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no active session").into_response(),
    }
}

async fn handle_post_ln_invoice(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    body: String,
) -> Response {
    let amount: u64 = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("amount").and_then(|a| a.as_u64()))
        .unwrap_or(8);

    let mint_url = config
        .accepted_mints
        .first()
        .map(|m| m.url.as_str())
        .unwrap_or("http://localhost:3338");

    let wallet = match crate::v1_compat::wallet::CdkWallet::new(mint_url, [0u8; 64]).await {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("wallet init failed: {e}"),
            )
                .into_response()
        }
    };

    match wallet.request_mint_quote(amount, mint_url).await {
        Ok(info) => {
            let json = serde_json::json!({
                "status": 0,
                "quote": info.quote_id,
                "invoice": info.invoice,
                "amount": info.amount,
                "expiry": info.expiry,
            });
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mint quote failed: {e}"),
        )
            .into_response(),
    }
}

async fn handle_get_ln_invoice(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let quote_id = match params.get("quote") {
        Some(id) => id.as_str(),
        None => return (StatusCode::BAD_REQUEST, "missing quote parameter").into_response(),
    };

    let mint_url = config
        .accepted_mints
        .first()
        .map(|m| m.url.as_str())
        .unwrap_or("http://localhost:3338");

    let wallet = match crate::v1_compat::wallet::CdkWallet::new(mint_url, [0u8; 64]).await {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("wallet init failed: {e}"),
            )
                .into_response()
        }
    };

    match wallet.check_mint_quote_status(quote_id).await {
        Ok(state) => {
            let state_str = match state {
                crate::v1_compat::wallet::QuoteState::Unpaid => "UNPAID",
                crate::v1_compat::wallet::QuoteState::Paid => "PAID",
                crate::v1_compat::wallet::QuoteState::Issued => "ISSUED",
            };
            let json = serde_json::json!({
                "quote": quote_id,
                "state": state_str,
            });
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("quote check failed: {e}"),
        )
            .into_response(),
    }
}

fn extract_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
        })
}

fn extract_ip_full(
    headers: &HeaderMap,
    conn: Option<&axum::extract::ConnectInfo<std::net::SocketAddr>>,
) -> Option<std::net::IpAddr> {
    extract_ip(headers).or_else(|| conn.map(|c| c.0.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Identity};
    use crate::wallet::BootstrapWallet;
    use crate::adapter::IpAdapter;
    use axum::body::Body;
    use axum::http::Request;
    use nostr::key::Keys;
    use tollgate_core::Price;
    use tower::ServiceExt;

    fn test_config() -> Arc<V1ServerConfig> {
        Arc::new(V1ServerConfig {
            metric: "milliseconds".to_string(),
            step_size: 60_000,
            accepted_mints: vec![merchant::AcceptedMint {
                url: String::new(),
                price_per_step: 1,
                unit: "sat".to_string(),
                min_steps: 1,
            }],
            nostr_keys: Keys::generate(),
        })
    }

    fn test_driver() -> Driver {
        let identity = Arc::new(Identity::load_or_generate(&Config::default()).unwrap());
        Driver::new(
            BootstrapWallet::new(vec![]),
            IpAdapter::new(),
            identity,
            Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn get_advertisement_returns_nostr_10021() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 10021);
        assert!(json["tags"].is_array());
    }

    #[tokio::test]
    async fn get_usage_returns_404_without_session() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_balance_returns_404_without_session() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/balance")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_ln_invoice_attempts_wallet_init() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ln-invoice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::SERVICE_UNAVAILABLE
                || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
            "expected 503/500 from wallet init failure, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn post_payment_rejects_invalid_token() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from("not-a-token"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn advertisement_has_metric_and_step_size_tags() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tags = json["tags"].as_array().unwrap();
        let has_metric = tags.iter().any(|t| t.as_array().unwrap().first().unwrap() == "metric");
        let has_step = tags.iter().any(|t| t.as_array().unwrap().first().unwrap() == "step_size");
        assert!(has_metric, "advertisement must have metric tag");
        assert!(has_step, "advertisement must have step_size tag");
    }

    #[tokio::test]
    async fn advertisement_has_price_per_step_tag() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tags = json["tags"].as_array().unwrap();
        let has_price = tags.iter().any(|t| {
            t.as_array().unwrap().first().unwrap() == "price_per_step"
        });
        assert!(has_price, "advertisement must have price_per_step tag");
    }

    #[tokio::test]
    async fn post_payment_rejects_empty_body() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_payment_accepts_json_token_format() {
        let app = build_router(test_driver(), test_config());
        let body = serde_json::json!({"token": "cashuBnotreal"}).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::BAD_REQUEST
                || resp.status() == StatusCode::PAYMENT_REQUIRED,
            "expected 400 or 402 for invalid token in JSON, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn get_pay_alias_returns_advertisement() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(Request::builder().method("GET").uri("/pay").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 10021);
    }

    #[tokio::test]
    async fn whoami_returns_mac_format_with_connectinfo() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/whoami")
                    .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 100], 12345))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 1024).await.unwrap().to_vec(),
        )
        .unwrap();
        assert!(body.starts_with("mac="), "whoami must return 'mac=...' format, got: {body}");
    }

    #[tokio::test]
    async fn usage_returns_404_for_unknown_peer() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .extension(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn balance_returns_404_for_unknown_peer() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/balance")
                    .extension(ConnectInfo(SocketAddr::from(([10, 0, 0, 2], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
