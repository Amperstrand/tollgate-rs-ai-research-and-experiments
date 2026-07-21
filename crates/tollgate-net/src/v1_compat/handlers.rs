use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;

use crate::driver::Driver;
use crate::v1_compat::adapter;
use crate::v1_compat::merchant::{self, V1ServerConfig};

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    let json = serde_json::json!({"error": message.into()});
    (
        status,
        [("content-type", "application/json")],
        serde_json::to_string(&json).unwrap_or_default(),
    )
        .into_response()
}

fn extract_token_amount_and_allotment(token: &str, config: &V1ServerConfig) -> (u64, u64) {
    let amount = decode_cashu_amount(token).unwrap_or(0);
    if amount == 0 {
        return (0, 0);
    }
    let mint_url = config
        .accepted_mints
        .first()
        .map(|m| m.url.as_str())
        .unwrap_or("");
    let allotment = merchant::calculate_allotment(amount, mint_url, config).unwrap_or(0);
    (amount, allotment)
}

fn decode_cashu_amount(token: &str) -> Option<u64> {
    let payload = token
        .strip_prefix("cashuA")
        .or_else(|| token.strip_prefix("cashua"))?;
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let amount = json
        .get("token")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| entry.get("proofs").and_then(|p| p.as_array()))
                .flatten()
                .filter_map(|proof| proof.get("amount").and_then(|a| a.as_u64()))
                .sum::<u64>()
        })
        .unwrap_or(0);
    Some(amount)
}

pub fn build_router(driver: Driver, config: Arc<V1ServerConfig>) -> Router {
    Router::new()
        .route("/", get(handle_get_details).post(handle_post_payment))
        .route("/pay", get(handle_get_details))
        .route("/usage", get(handle_usage))
        .route("/whoami", get(handle_whoami))
        .route("/balance", get(handle_balance))
        .route(
            "/ln-invoice",
            get(handle_get_ln_invoice).post(handle_post_ln_invoice),
        )
        .layer(axum::Extension(config))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(driver)
}

fn extract_payment_token(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.starts_with("cashu")
        || trimmed.starts_with("cashuA")
        || trimmed.starts_with("cashuB")
    {
        return Some(trimmed.to_owned());
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(token) = json.get("token").and_then(|t| t.as_str()) {
            return Some(token.to_owned());
        }
    }
    None
}

fn extract_mint_from_token(_token: &str) -> Option<String> {
    None
}

async fn handle_get_details(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
) -> Response {
    match merchant::build_advertisement(&config) {
        Ok(json) => (StatusCode::OK, [("content-type", "application/json")], json).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to build advertisement: {e}"),
        ),
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
            let json = serde_json::json!({"error": "missing or invalid token"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    let client_ip = extract_ip(&headers, config.trust_proxy_headers);

    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(hex) => hex,
        None => {
            let json = serde_json::json!({"error": "could not resolve client identity"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    let result = adapter::admit_peer_with_token(&driver, &peer_hex, client_ip, &token).await;

    if result.accepted {
        let (amount_sat, allotment) = extract_token_amount_and_allotment(&token, &config);

        let session = merchant::CustomerSession {
            mac_address: peer_hex.clone(),
            start_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            metric: config.metric.clone(),
            allotment,
        };
        match merchant::build_session_event(&session, &config, &peer_hex, amount_sat, "cashu") {
            Ok(event_json) => (
                StatusCode::OK,
                [("content-type", "application/json")],
                event_json,
            )
                .into_response(),
            Err(e) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("session event error: {e}"),
            ),
        }
    } else {
        if let Some(tracker) = &config.mint_health {
            let reason = result.reason.as_deref().unwrap_or("");
            if !reason.contains("spent") && !reason.contains("Spent") {
                if let Some(mint_url) = extract_mint_from_token(&token) {
                    tracker.mark_unreachable(&mint_url);
                    tracing::warn!(mint = %mint_url, "marked mint unreachable due to payment failure");
                }
            }
        }
        let notice = merchant::build_notice_event(
            "error",
            "payment_rejected",
            result.reason.as_deref().unwrap_or("rejected"),
            Some(&peer_hex),
            &config,
        );
        match notice {
            Ok(json) => (
                StatusCode::PAYMENT_REQUIRED,
                [("content-type", "application/json")],
                json,
            )
                .into_response(),
            Err(_) => json_error(StatusCode::PAYMENT_REQUIRED, "payment rejected"),
        }
    }
}

async fn handle_usage(
    State(driver): State<Driver>,
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(
        &headers,
        Some(&ConnectInfo(addr)),
        config.trust_proxy_headers,
    );
    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(h) => h,
        None => {
            let json = serde_json::json!({"error": "unknown client"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };
    match adapter::get_usage_text(&driver, &peer_hex).await {
        Some(text) => (StatusCode::OK, text).into_response(),
        None => {
            let json = serde_json::json!({"error": "no active session"});
            (
                StatusCode::NOT_FOUND,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
    }
}

async fn handle_whoami(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(
        &headers,
        Some(&ConnectInfo(addr)),
        config.trust_proxy_headers,
    );
    let ip_str = client_ip.map(|ip| ip.to_string()).unwrap_or_default();

    let resolver = crate::v1_compat::mac_resolver::DhcpLeasesResolver;
    if let Ok(mac) = crate::v1_compat::mac_resolver::MacResolver::resolve(&resolver, &ip_str) {
        return (StatusCode::OK, format!("mac={mac}")).into_response();
    }

    json_error(StatusCode::BAD_REQUEST, "unknown client")
}

async fn handle_balance(
    State(driver): State<Driver>,
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(
        &headers,
        Some(&ConnectInfo(addr)),
        config.trust_proxy_headers,
    );
    let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
        Some(h) => h,
        None => {
            let json = serde_json::json!({"error": "unknown client"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };
    match adapter::get_balance_json(&driver, &peer_hex).await {
        Some(json) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::to_string(&json).unwrap_or_default(),
        )
            .into_response(),
        None => {
            let json = serde_json::json!({"error": "no active session"});
            (
                StatusCode::NOT_FOUND,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
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

    let wallet = match config.wallet.as_ref() {
        Some(w) => w.clone(),
        None => return json_error(StatusCode::SERVICE_UNAVAILABLE, "wallet not configured"),
    };

    let mint_url = config
        .accepted_mints
        .first()
        .map(|m| m.url.as_str())
        .unwrap_or("http://localhost:3338");

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
        Err(e) => {
            let json =
                serde_json::json!({"error": format!("mint quote failed: {e}"), "amount": amount});
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
    }
}

async fn handle_get_ln_invoice(
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let quote_id = match params.get("quote") {
        Some(id) => id.as_str(),
        None => {
            let json = serde_json::json!({"error": "missing quote parameter"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    let wallet = match config.wallet.as_ref() {
        Some(w) => w.clone(),
        None => return json_error(StatusCode::SERVICE_UNAVAILABLE, "wallet not configured"),
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
        Err(e) => {
            let json =
                serde_json::json!({"error": format!("quote check failed: {e}"), "quote": quote_id});
            let status = if format!("{e}").to_lowercase().contains("unknown") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response()
        }
    }
}

fn extract_ip(headers: &HeaderMap, trust: bool) -> Option<std::net::IpAddr> {
    if trust {
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
    } else {
        None
    }
}

fn extract_ip_full(
    headers: &HeaderMap,
    conn: Option<&axum::extract::ConnectInfo<std::net::SocketAddr>>,
    trust: bool,
) -> Option<std::net::IpAddr> {
    extract_ip(headers, trust).or_else(|| conn.map(|c| c.0.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::IpAdapter;
    use crate::config::{Config, Identity};
    use crate::wallet::BootstrapWallet;
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
            trust_proxy_headers: false,
            wallet: None,
            mint_health: None,
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

    #[test]
    fn extract_ip_ignores_xff_when_untrusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let ip = extract_ip(&headers, false);
        assert!(
            ip.is_none(),
            "extract_ip must ignore XFF when trust=false, got: {ip:?}"
        );
    }

    #[test]
    fn extract_ip_honors_xff_when_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let ip = extract_ip(&headers, true);
        assert_eq!(ip, Some("1.2.3.4".parse().unwrap()));
    }

    #[tokio::test]
    async fn get_advertisement_returns_nostr_10021() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 10021);
        assert!(json["tags"].is_array());
    }

    #[tokio::test]
    async fn get_usage_returns_400_without_known_peer() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_balance_returns_400_without_known_peer() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
    async fn post_ln_invoice_returns_503_with_explicit_message_when_wallet_not_configured() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ln-invoice")
                    .body(Body::from(r#"{"amount":8}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "POST /ln-invoice must return 503 when wallet is not configured"
        );
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("ln-invoice 503 response must be valid JSON");
        assert_eq!(
            json["error"], "wallet not configured",
            "POST /ln-invoice 503 must explain 'wallet not configured' (B2 contract); got: {json}"
        );
    }

    #[tokio::test]
    async fn get_ln_invoice_returns_503_with_explicit_message_when_wallet_not_configured() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ln-invoice?quote=abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "GET /ln-invoice must return 503 when wallet is not configured"
        );
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("ln-invoice 503 response must be valid JSON");
        assert_eq!(
            json["error"], "wallet not configured",
            "GET /ln-invoice 503 must explain 'wallet not configured' (B2 contract); got: {json}"
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
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tags = json["tags"].as_array().unwrap();
        let has_metric = tags
            .iter()
            .any(|t| t.as_array().unwrap().first().unwrap() == "metric");
        let has_step = tags
            .iter()
            .any(|t| t.as_array().unwrap().first().unwrap() == "step_size");
        assert!(has_metric, "advertisement must have metric tag");
        assert!(has_step, "advertisement must have step_size tag");
    }

    #[tokio::test]
    async fn advertisement_has_price_per_step_tag() {
        let app = build_router(test_driver(), test_config());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tags = json["tags"].as_array().unwrap();
        let has_price = tags
            .iter()
            .any(|t| t.as_array().unwrap().first().unwrap() == "price_per_step");
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
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/pay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 10021);
    }

    #[tokio::test]
    async fn whoami_returns_400_when_peer_unknown() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            body.contains("unknown client"),
            "whoami must return JSON 'unknown client' for unregistered peer, got: {body}"
        );
    }

    #[tokio::test]
    async fn usage_returns_400_for_unknown_peer() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn balance_returns_400_for_unknown_peer() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_with_large_body_returns_413() {
        let app = build_router(test_driver(), test_config());
        let big_body = vec![b'A'; 70_000]; // 70KB > 64KB limit
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from(big_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "70KB body should be rejected by 64KB limit"
        );
    }

    #[tokio::test]
    async fn advertisement_error_returns_json_content_type() {
        let resp = json_error(StatusCode::BAD_REQUEST, "test error");
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "json_error must set content-type to application/json, got: {ct}"
        );
    }

    #[tokio::test]
    async fn advertisement_returns_degraded_when_all_mints_unreachable() {
        use crate::v1_compat::mint_health::MintHealthTracker;

        let mint_url = "http://down.example".to_string();
        let tracker =
            std::sync::Arc::new(MintHealthTracker::new(&[mint_url.clone()]));
        tracker.mark_unreachable(&mint_url);
        assert!(tracker.all_unreachable());

        let config = std::sync::Arc::new(V1ServerConfig {
            metric: "milliseconds".to_string(),
            step_size: 60_000,
            accepted_mints: vec![merchant::AcceptedMint {
                url: mint_url,
                price_per_step: 1,
                unit: "sat".to_string(),
                min_steps: 1,
            }],
            nostr_keys: Keys::generate(),
            trust_proxy_headers: false,
            wallet: None,
            mint_health: Some(tracker),
        });

        let app = build_router(test_driver(), config);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("degraded advertisement must be valid JSON");
        assert_eq!(json["kind"], 10021, "degraded advertisement keeps kind 10021");
        assert_eq!(
            json["content"],
            "TollGate is in degraded mode. No reachable mints detected."
        );

        let tags = json["tags"]
            .as_array()
            .expect("degraded advertisement must have tags array");
        let has_warning = tags.iter().any(|t| {
            let arr = t.as_array().unwrap();
            arr.first().and_then(|s| s.as_str()) == Some("level")
                && arr.get(1).and_then(|s| s.as_str()) == Some("warning")
        });
        let has_code = tags.iter().any(|t| {
            let arr = t.as_array().unwrap();
            arr.first().and_then(|s| s.as_str()) == Some("code")
                && arr.get(1).and_then(|s| s.as_str()) == Some("no-reachable-mints")
        });
        let has_price = tags.iter().any(|t| {
            t.as_array().unwrap().first().and_then(|s| s.as_str())
                == Some("price_per_step")
        });
        assert!(has_warning, "degraded advertisement must carry level=warning");
        assert!(
            has_code,
            "degraded advertisement must carry code=no-reachable-mints"
        );
        assert!(
            !has_price,
            "degraded advertisement must NOT advertise price_per_step"
        );
    }
}
