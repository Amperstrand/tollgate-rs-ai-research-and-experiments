//! Axum route handlers for the v1 HTTP/JSON TollGate API (port 2121).
//!
//! Endpoints (mirroring the Go v1 server):
//! - `GET  /`        → Nostr kind 10021 advertisement
//! - `POST /`        → Cashu token → kind 1022 session or kind 21023 notice
//! - `GET  /usage`   → text `"<used>/<allotment>"` or `"-1/-1"`
//! - `GET  /balance` → JSON `{"remaining": …, "allotment": …, …}`
//! - `GET  /whoami`  → text `"mac=<MAC>"`
//! - `OPTIONS /`     → CORS preflight (empty 200)

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use secp256k1::SecretKey;

use crate::adapter::IpAdapter;
use crate::wallet::BootstrapWallet;

use super::nostr::{KIND_ADVERTISEMENT, KIND_NOTICE, KIND_SESSION, build_event, event_to_json};
use super::session::{V1Session, V1SessionStore, now_unix};

/// Static pricing / advertisement configuration for the v1 server.
pub struct V1Config {
    /// Metering metric: `"milliseconds"` or `"bytes"`.
    pub metric: String,
    /// Step size in metric units (e.g. 60 000 for 1-minute steps in ms).
    pub step_size: u64,
    /// Price per step in sats.
    pub price_per_step: u64,
    /// Currency unit for pricing (e.g. `"sat"`).
    pub unit: String,
    /// Primary mint URL (used in advertisement and accepted-mint check).
    pub mint_url: String,
    /// Minimum number of steps purchasable in a single payment.
    pub min_steps: u64,
    /// Supported TIP numbers (tagged in the advertisement).
    pub tips: Vec<String>,
}

/// Shared state passed to every v1 handler via Axum's `State` extractor.
pub struct V1State {
    /// Precomputed kind 10021 advertisement JSON (rebuilt only on startup).
    pub advertisement: String,
    /// Raw secret-key bytes for signing Nostr events.
    pub secret_key_bytes: [u8; 32],
    /// X-only pubkey hex (32 bytes → 64 chars) — the `pubkey` field in events.
    pub xonly_pubkey_hex: String,
    /// Cashu token verifier.
    pub wallet: BootstrapWallet,
    /// Firewall adapter for IP allow/deny.
    pub adapter: IpAdapter,
    /// In-memory session store keyed by lowercase MAC.
    pub sessions: V1SessionStore,
    /// Static pricing / advertisement config.
    pub config: V1Config,
}

/// Build the Axum router with all v1 routes mounted.
pub fn build_router(state: Arc<V1State>) -> Router {
    Router::new()
        .route(
            "/",
            axum::routing::get(handle_get_details)
                .post(handle_post_payment)
                .options(handle_options),
        )
        .route(
            "/usage",
            axum::routing::get(handle_usage).options(handle_options),
        )
        .route(
            "/balance",
            axum::routing::get(handle_balance).options(handle_options),
        )
        .route(
            "/whoami",
            axum::routing::get(handle_whoami).options(handle_options),
        )
        .with_state(state)
        // Go v1 parity: cap request body at 1 MB.
        .layer(DefaultBodyLimit::max(1_048_576))
}

// -----------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------

/// `GET /` — return the precomputed advertisement (kind 10021).
async fn handle_get_details(headers: HeaderMap, State(state): State<Arc<V1State>>) -> Response {
    let (origin, is_local) = resolve_origin(&headers);
    cors_response(
        json_response(StatusCode::OK, state.advertisement.clone()),
        origin.as_deref(),
        is_local,
    )
}

/// `POST /` — accept a Cashu token (raw or wrapped in a kind 21000 event),
/// verify it with the mint, create a session, open the firewall, and return a
/// kind 1022 session event.  On failure, return a kind 21023 notice.
#[allow(clippy::too_many_lines)]
async fn handle_post_payment(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<V1State>>,
    body: String,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers);

    let secret_key = match SecretKey::from_slice(&state.secret_key_bytes) {
        Ok(sk) => sk,
        Err(e) => {
            tracing::error!(err = %e, "internal: secret key invalid");
            return cors_response(
                notice_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal-error",
                    "server identity error",
                    None,
                    &state,
                    &secret_key_placeholder(),
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let mac = resolve_mac_from_headers(&headers, &ip);

    let payment = extract_payment_token(&body);

    let amount_milli = match state.wallet.verify(&payment.token).await {
        Ok(amt) => amt,
        Err(e) => {
            tracing::warn!(err = %e, %mac, "token rejected");
            let code = classify_payment_error(&e.to_string());
            return cors_response(
                notice_response(
                    StatusCode::BAD_REQUEST,
                    code,
                    &format!("payment rejected: {e}"),
                    Some(&mac),
                    &state,
                    &secret_key,
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let amount_sat = amount_milli / 1000;
    let allotment = calculate_allotment(amount_sat, &state.config);

    let session = state
        .sessions
        .top_up(&mac, &state.config.metric, allotment, amount_sat)
        .await;

    let client_ip: IpAddr = ip
        .parse()
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    state.adapter.allow(client_ip);
    tracing::info!(%mac, %client_ip, allotment, amount_sat, "payment accepted");

    let event = build_session_event(&session, &state, &mac, amount_sat, &secret_key);
    match event_to_json(&event) {
        Ok(json) => cors_response(
            json_response(StatusCode::OK, json),
            origin.as_deref(),
            is_local,
        ),
        Err(e) => {
            tracing::error!(err = %e, "failed to serialize session event");
            cors_response(
                notice_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal-error",
                    "serialization error",
                    Some(&mac),
                    &state,
                    &secret_key,
                ),
                origin.as_deref(),
                is_local,
            )
        }
    }
}

/// `GET /usage` — text `"<elapsed_ms>/<allotment_ms>"` or `"-1/-1"`.
async fn handle_usage(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<V1State>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers);
    let mac = resolve_mac_from_headers(&headers, &ip);

    let session = match state.sessions.get(&mac).await {
        Some(s) => s,
        None => {
            return cors_response(
                text_response(StatusCode::OK, "-1/-1"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    if session.metric == "milliseconds" {
        let elapsed = session.elapsed_ms();
        if elapsed >= session.allotment as i64 {
            expire_session(&state, &mac, &ip).await;
            return cors_response(
                text_response(StatusCode::OK, "-1/-1"),
                origin.as_deref(),
                is_local,
            );
        }
        cors_response(
            text_response(StatusCode::OK, format!("{elapsed}/{}", session.allotment)),
            origin.as_deref(),
            is_local,
        )
    } else {
        let usage = state
            .adapter
            .read_counters(
                ip.parse()
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            )
            .delivered;
        if usage >= session.allotment {
            expire_session(&state, &mac, &ip).await;
            return cors_response(
                text_response(StatusCode::OK, "-1/-1"),
                origin.as_deref(),
                is_local,
            );
        }
        cors_response(
            text_response(StatusCode::OK, format!("{usage}/{}", session.allotment)),
            origin.as_deref(),
            is_local,
        )
    }
}

/// `GET /balance` — JSON with remaining/allotment/usage/start_time.
async fn handle_balance(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<V1State>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers);
    let mac = resolve_mac_from_headers(&headers, &ip);

    let session = match state.sessions.get(&mac).await {
        Some(s) => s,
        None => {
            return cors_response(
                json_response(StatusCode::OK, r#"{"remaining":0}"#.to_string()),
                origin.as_deref(),
                is_local,
            );
        }
    };

    if session.metric == "milliseconds" {
        let elapsed = session.elapsed_ms();
        if elapsed >= session.allotment as i64 {
            expire_session(&state, &mac, &ip).await;
            return cors_response(
                json_response(StatusCode::OK, r#"{"remaining":0}"#.to_string()),
                origin.as_deref(),
                is_local,
            );
        }
        let remaining = (session.allotment as i64 - elapsed).max(0) as u64;
        let json = serde_json::json!({
            "remaining": remaining,
            "allotment": session.allotment,
            "metric": session.metric,
        });
        cors_response(
            json_response(StatusCode::OK, json.to_string()),
            origin.as_deref(),
            is_local,
        )
    } else {
        let usage = state
            .adapter
            .read_counters(
                ip.parse()
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            )
            .delivered;
        if usage >= session.allotment {
            expire_session(&state, &mac, &ip).await;
            return cors_response(
                json_response(StatusCode::OK, r#"{"remaining":0}"#.to_string()),
                origin.as_deref(),
                is_local,
            );
        }
        let remaining = session.allotment.saturating_sub(usage);
        let json = serde_json::json!({
            "remaining": remaining,
            "allotment": session.allotment,
            "metric": session.metric,
        });
        cors_response(
            json_response(StatusCode::OK, json.to_string()),
            origin.as_deref(),
            is_local,
        )
    }
}

/// `GET /whoami` — text `"mac=<MAC>"`.
async fn handle_whoami(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(_state): State<Arc<V1State>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers);
    let mac = resolve_mac_from_headers(&headers, &ip);
    cors_response(
        text_response(StatusCode::OK, format!("mac={mac}")),
        origin.as_deref(),
        is_local,
    )
}

/// `OPTIONS /` — CORS preflight, empty 200.
async fn handle_options(headers: HeaderMap) -> Response {
    let (origin, is_local) = resolve_origin(&headers);
    cors_response(StatusCode::OK.into_response(), origin.as_deref(), is_local)
}

// -----------------------------------------------------------------------
// Event building helpers
// -----------------------------------------------------------------------

/// Build the precomputed advertisement JSON (kind 10021).
pub fn build_advertisement_json(
    pubkey_hex: &str,
    secret_key: &SecretKey,
    config: &V1Config,
) -> anyhow::Result<String> {
    let tips_tag: Vec<String> = std::iter::once("tips".to_string())
        .chain(config.tips.iter().cloned())
        .collect();

    let tags: Vec<Vec<String>> = vec![
        vec!["metric".into(), config.metric.clone()],
        vec!["step_size".into(), config.step_size.to_string()],
        vec![
            "price_per_step".into(),
            "cashu".into(),
            config.price_per_step.to_string(),
            config.unit.clone(),
            config.mint_url.clone(),
            config.min_steps.to_string(),
        ],
        tips_tag,
    ];

    let event = build_event(
        KIND_ADVERTISEMENT,
        tags,
        "",
        now_unix() as u64,
        pubkey_hex,
        secret_key,
    );
    Ok(event_to_json(&event)?)
}

/// Build a session event (kind 1022) for a successful payment.
fn build_session_event(
    session: &V1Session,
    state: &V1State,
    mac: &str,
    amount_sat: u64,
    secret_key: &SecretKey,
) -> super::nostr::NostrEvent {
    let tags = vec![
        vec!["allotment".into(), session.allotment.to_string()],
        vec!["metric".into(), session.metric.clone()],
        vec!["start-time".into(), session.start_time.to_string()],
        vec!["device-identifier".into(), "mac".into(), mac.to_string()],
        vec!["p".into(), mac.to_string()],
        vec!["amount_sat".into(), amount_sat.to_string()],
    ];

    build_event(
        KIND_SESSION,
        tags,
        "",
        session.start_time as u64,
        &state.xonly_pubkey_hex,
        secret_key,
    )
}

/// Build a notice event (kind 21023) for error responses.
fn notice_response(
    status: StatusCode,
    code: &str,
    message: &str,
    customer_identifier: Option<&str>,
    state: &V1State,
    secret_key: &SecretKey,
) -> Response {
    let mut tags: Vec<Vec<String>> = vec![
        vec!["level".into(), "error".into()],
        vec!["code".into(), code.into()],
    ];
    if let Some(id) = customer_identifier {
        if !id.is_empty() {
            tags.push(vec!["p".into(), id.into()]);
        }
    }

    let event = build_event(
        KIND_NOTICE,
        tags,
        message,
        now_unix() as u64,
        &state.xonly_pubkey_hex,
        secret_key,
    );

    match event_to_json(&event) {
        Ok(json) => json_response(status, json),
        Err(e) => {
            tracing::error!(err = %e, "failed to serialize notice event");
            text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("internal error: {e}"),
            )
        }
    }
}

// -----------------------------------------------------------------------
// Utility functions
// -----------------------------------------------------------------------

/// Calculate allotment in metric units (ms) from the amount paid in sats.
fn calculate_allotment(amount_sat: u64, config: &V1Config) -> u64 {
    if config.price_per_step == 0 {
        return config.step_size;
    }
    let steps = amount_sat / config.price_per_step;
    steps * config.step_size
}

/// Extract the client IP from `X-Forwarded-For` or `X-Real-IP`, falling back to
/// the connection's source address.
pub fn extract_client_ip(
    connect_info: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
) -> String {
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(xff_str) = xff.to_str() {
            if let Some(first_ip) = xff_str.split(',').next() {
                let trimmed = first_ip.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }
    }

    if let Some(xri) = headers.get("x-real-ip") {
        if let Ok(ip) = xri.to_str() {
            let trimmed = ip.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }

    connect_info
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_default()
}

/// Resolve a MAC address for the given IP, trying (in order): the
/// `X-TollGate-MAC` header, `/tmp/dhcp.leases`, then falling back to the raw
/// IP as an identifier (for testing).
fn resolve_mac_from_headers(headers: &HeaderMap, ip: &str) -> String {
    if let Some(header_mac) = headers
        .get("x-tollgate-mac")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return header_mac.to_lowercase().replace(['-', '.'], ":");
    }

    resolve_mac(ip)
}

/// Resolve IP → MAC via `/tmp/dhcp.leases` (OpenWrt format), or fall back to
/// the raw IP as an identifier.
pub fn resolve_mac(ip: &str) -> String {
    if let Ok(contents) = std::fs::read_to_string("/tmp/dhcp.leases") {
        for line in contents.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[2] == ip {
                return parts[1].to_lowercase().replace('-', ":");
            }
        }
    }
    ip.to_lowercase()
}

/// Information extracted from a POST body: the Cashu token, plus optionally the
/// customer's Nostr pubkey and MAC from a kind 21000 event wrapper.
struct ExtractedPayment {
    token: String,
    #[allow(dead_code)]
    customer_pubkey: Option<String>,
    #[allow(dead_code)]
    mac_from_event: Option<String>,
}

/// Extract the Cashu token from the POST body.  The body may be:
/// 1. A raw Cashu token string (Content-Type: text/plain)
/// 2. A JSON kind 21000 Nostr event with a `payment` tag (Content-Type: application/json)
fn extract_payment_token(body: &str) -> ExtractedPayment {
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(body) {
        if event.get("kind").and_then(|k| k.as_u64()) == Some(21_000) {
            let token = event
                .get("tags")
                .and_then(|t| t.as_array())
                .and_then(|tags| {
                    tags.iter().find_map(|tag| {
                        let items = tag.as_array()?;
                        if items.first()?.as_str()? == "payment" {
                            items.get(1)?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                });
            let customer_pubkey = event
                .get("pubkey")
                .and_then(|p| p.as_str())
                .map(String::from);
            let mac_from_event = event
                .get("tags")
                .and_then(|t| t.as_array())
                .and_then(|tags| {
                    tags.iter().find_map(|tag| {
                        let items = tag.as_array()?;
                        if items.first()?.as_str()? == "device-identifier" {
                            items.get(2)?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                });
            if let Some(token) = token {
                return ExtractedPayment {
                    token,
                    customer_pubkey,
                    mac_from_event,
                };
            }
        }
    }
    ExtractedPayment {
        token: body.trim().to_string(),
        customer_pubkey: None,
        mac_from_event: None,
    }
}

/// Classify a wallet error into a Go v1-compatible error code.
fn classify_payment_error(err_str: &str) -> &'static str {
    let lower = err_str.to_ascii_lowercase();
    if lower.contains("already spent") || lower.contains("token already") {
        "payment-error-token-spent"
    } else if lower.contains("invalid token")
        || lower.contains("decode")
        || lower.contains("token rejected")
        || lower.contains("too short")
        || lower.contains("no proofs")
    {
        "payment-error-invalid-token"
    } else {
        "payment-processing-failed"
    }
}

/// Remove an expired session and deny the client at the firewall.
async fn expire_session(state: &V1State, mac: &str, ip: &str) {
    state.sessions.remove(mac).await;
    let client_ip: IpAddr = ip
        .parse()
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    state.adapter.deny(client_ip);
    tracing::info!(%mac, %client_ip, "session expired");
}

/// Extract the Origin header from the request.
fn get_origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
}

/// Check whether the Origin refers to a local/private address.
/// Allows CORS for management UIs on the LAN but blocks public origins.
fn is_local_origin(origin: &str) -> bool {
    let host_part = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or(origin);

    // IPv6 addresses in URLs are bracketed, e.g. `http://[fd00::1]:8080`.
    if host_part.starts_with('[') {
        let end = host_part.find(']').unwrap_or(host_part.len());
        let host = &host_part[1..end];
        if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
            return ip.is_loopback() || matches!(ip.octets(), [0xfd, ..]);
        }
        return false;
    }

    let host = host_part.split(':').next().unwrap_or(host_part);

    if host == "localhost" {
        return true;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback() || matches!(v6.octets(), [0xfd, ..]),
        };
    }

    false
}

fn resolve_origin(headers: &HeaderMap) -> (Option<String>, bool) {
    let origin = get_origin(headers);
    let is_local = origin.as_deref().map(is_local_origin).unwrap_or(false);
    (origin, is_local)
}

/// Add CORS headers to a response.  Only local/private origins get the
/// `Access-Control-Allow-Origin` echo; the methods/headers are always added.
fn cors_response(mut response: Response, origin: Option<&str>, is_local: bool) -> Response {
    let headers = response.headers_mut();
    if is_local {
        if let Some(origin_val) = origin {
            if let Ok(hv) = HeaderValue::from_str(origin_val) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, hv);
            }
        }
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type, Authorization"),
    );
    response
}

fn json_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response()
}

fn text_response(status: StatusCode, body: impl Into<String>) -> Response {
    (status, body.into()).into_response()
}

/// Placeholder secret key for the error-in-error path (never used for signing).
fn secret_key_placeholder() -> SecretKey {
    SecretKey::from_slice(&[0u8; 32]).unwrap_or_else(|_| {
        // [0; 32] is not a valid secp256k1 secret key (it's the curve identity);
        // use [1; 32] which is always valid.
        SecretKey::from_slice(&[1u8; 32]).expect("non-zero key is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ip_prefers_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        assert_eq!(extract_client_ip(None, &headers), "1.2.3.4");
    }

    #[test]
    fn extract_ip_falls_back_to_connect_info() {
        let ci = ConnectInfo(SocketAddr::new("10.0.0.1".parse().unwrap(), 1234));
        let headers = HeaderMap::new();
        assert_eq!(extract_client_ip(Some(&ci), &headers), "10.0.0.1");
    }

    #[test]
    fn extract_ip_trims_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "  1.2.3.4  ".parse().unwrap());
        assert_eq!(extract_client_ip(None, &headers), "1.2.3.4");
    }

    #[test]
    fn resolve_mac_falls_back_to_ip() {
        // No /tmp/dhcp.leases on test machine → fallback to IP.
        let mac = resolve_mac("192.168.1.100");
        assert_eq!(mac, "192.168.1.100");
    }

    #[test]
    fn extract_payment_raw_token() {
        let payment = extract_payment_token("cashuBxyz123");
        assert_eq!(payment.token, "cashuBxyz123");
    }

    #[test]
    fn extract_payment_from_nostr_event() {
        let body = r#"{
            "kind": 21000,
            "pubkey": "abcdef",
            "tags": [["payment", "cashuBtoken"], ["device-identifier", "mac", "00:11:22:33:44:55"]],
            "content": ""
        }"#;
        let payment = extract_payment_token(body);
        assert_eq!(payment.token, "cashuBtoken");
        assert_eq!(payment.customer_pubkey.as_deref(), Some("abcdef"));
        assert_eq!(payment.mac_from_event.as_deref(), Some("00:11:22:33:44:55"));
    }

    #[test]
    fn calculate_allotment_basic() {
        let config = V1Config {
            metric: "milliseconds".into(),
            step_size: 60_000,
            price_per_step: 1,
            unit: "sat".into(),
            mint_url: "https://testnut.cashu.exchange".into(),
            min_steps: 1,
            tips: (1..=10).map(|i| i.to_string()).collect(),
        };
        // 1 sat × 60 000 ms/step ÷ 1 sat/step = 60 000 ms
        assert_eq!(calculate_allotment(1, &config), 60_000);
        assert_eq!(calculate_allotment(8, &config), 480_000);
    }

    #[test]
    fn local_origin_allowed() {
        assert!(is_local_origin("http://localhost:3000"));
        assert!(is_local_origin("http://127.0.0.1:8080"));
        assert!(is_local_origin("http://192.168.1.1"));
        assert!(is_local_origin("http://10.0.0.1:2121"));
        assert!(is_local_origin("http://[fd00::1]"));
    }

    #[test]
    fn public_origin_blocked() {
        assert!(!is_local_origin("https://example.com"));
        assert!(!is_local_origin("http://8.8.8.8"));
    }

    #[test]
    fn classify_error_codes() {
        assert_eq!(
            classify_payment_error("one or more proofs are already spent"),
            "payment-error-token-spent",
        );
        assert_eq!(
            classify_payment_error("invalid token format"),
            "payment-error-invalid-token",
        );
        assert_eq!(
            classify_payment_error("network timeout"),
            "payment-processing-failed",
        );
    }
}
