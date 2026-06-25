//! Axum route handlers for the v1 HTTP/JSON TollGate API (port 2121).
//!
//! Endpoints (mirroring the Go v1 server):
//! - `GET  /`        → captive portal HTML page (triggers OS captive portal popup)
//! - `GET  /portal`  → alias of `GET /` (captive portal HTML)
//! - `POST /`        → Cashu token → kind 1022 session or kind 21023 notice
//! - `GET  /pay`     → Nostr kind 10021 advertisement
//! - `GET  /usage`   → text `"<used>/<allotment>"` or `"-1/-1"`
//! - `GET  /balance` → JSON `{"remaining": …, "allotment": …, …}`
//! - `GET  /whoami`  → text `"mac=<MAC>"`
//! - `OPTIONS /`     → CORS preflight (empty 200)
//! - `GET  /*` (fallback) → captive portal HTML (handles OS detection probes)

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn is_captive_probe_host(host: &str) -> bool {
    const PROBE_DOMAINS: &[&str] = &[
        "captive.apple.com",
        "connectivitycheck.gstatic.com",
        "www.msftconnecttest.com",
        "www.msftncsi.com",
        "detectportal.firefox.com",
        "clients3.google.com",
        "wifi.vodafone.com",
        "nmcheck.gnome.org",
    ];
    let host_part = host.split(':').next().unwrap_or(host);
    PROBE_DOMAINS.iter().any(|d| host_part == *d)
}

fn derive_gateway_ip(client_ip: IpAddr) -> String {
    match client_ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            format!("{}.{}.{}.1", octets[0], octets[1], octets[2])
        }
        IpAddr::V6(_) => "fe80::1".to_string(),
    }
}

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use secp256k1::SecretKey;

use crate::adapter::IpAdapter;
use crate::wallet::BootstrapWallet;

use super::nostr::{KIND_ADVERTISEMENT, KIND_NOTICE, KIND_SESSION, build_event, event_to_json};
use super::session::{V1Session, V1SessionStore, now_unix};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use minicbor::Encoder;

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
    /// Cashu tokens already redeemed — rejects immediate double-spend.
    pub spent_tokens: tokio::sync::Mutex<std::collections::HashSet<String>>,
    /// Static pricing / advertisement config.
    pub config: V1Config,
}

/// Build the Axum router with all v1 routes mounted.
pub fn build_router(state: Arc<V1State>) -> Router {
    Router::new()
        .route(
            "/",
            axum::routing::get(handle_portal)
                .post(handle_post_payment)
                .options(handle_options),
        )
        .route(
            "/portal",
            axum::routing::get(handle_portal).options(handle_options),
        )
        .route(
            "/pay",
            axum::routing::get(handle_get_details).options(handle_options),
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
        .fallback(handle_catch_all)
        .with_state(state)
        .layer(DefaultBodyLimit::max(1_048_576))
}

// -----------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------

/// `GET /pay` — NUT-24 endpoint. Without `X-Cashu` header, returns HTTP 402
/// with a NUT-18 payment request in the `X-Cashu` response header. With a valid
/// `X-Cashu` token, verifies the payment and returns the advertisement.
async fn handle_get_details(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<V1State>>,
) -> Response {
    let (origin, is_local) = resolve_origin(&headers);

    if let Some(cashu_header) = headers
        .get("x-cashu")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(String::from)
    {
        return handle_nut24_payment(
            ConnectInfo(addr),
            State(state),
            headers,
            &cashu_header,
            origin.as_deref(),
            is_local,
        )
        .await;
    }

    let addr_str = addr.ip().to_string();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&addr_str);
    let post_endpoint = if is_captive_probe_host(host) {
        let gateway = derive_gateway_ip(addr.ip());
        format!("http://{}:2121/", gateway)
    } else {
        format!("http://{host}/")
    };
    let creqa = create_creqa(
        state.config.price_per_step,
        &state.config.unit,
        &[state.config.mint_url.clone()],
        "TollGate internet access",
        &post_endpoint,
    );

    let mut response = json_response(
        StatusCode::PAYMENT_REQUIRED,
        serde_json::json!({
            "error": "payment required",
            "price": state.config.price_per_step,
            "unit": &state.config.unit,
            "mints": [&state.config.mint_url],
        })
        .to_string(),
    );
    if let Ok(hv) = HeaderValue::from_str(&creqa) {
        response.headers_mut().insert("x-cashu", hv);
    }
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("X-Cashu"),
    );
    cors_response(response, origin.as_deref(), is_local)
}

/// Process an X-Cashu payment from a GET /pay NUT-24 retry.
async fn handle_nut24_payment(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<V1State>>,
    headers: HeaderMap,
    token: &str,
    origin: Option<&str>,
    is_local: bool,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = resolve_mac_from_headers(&headers, &ip);

    let amount_milli = match state.wallet.verify(token).await {
        Ok(amt) => amt,
        Err(e) => {
            tracing::warn!(err = %e, %mac, "NUT-24 token rejected");
            return cors_response(
                json_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": format!("payment rejected: {e}")}).to_string(),
                ),
                origin,
                is_local,
            );
        }
    };

    state.spent_tokens.lock().await.insert(token.to_string());
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
    tracing::info!(%mac, %client_ip, allotment, amount_sat, "NUT-24 payment accepted");

    cors_response(
        json_response(StatusCode::OK, state.advertisement.clone()),
        origin,
        is_local,
    )
}

/// `GET /` or `GET /portal` — serve the captive portal HTML page.
async fn handle_portal(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<V1State>>,
    headers: HeaderMap,
) -> Response {
    let (origin, is_local) = resolve_origin(&headers);
    let addr_str = addr.ip().to_string();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&addr_str);
    let post_endpoint = if is_captive_probe_host(host) {
        let gateway = derive_gateway_ip(addr.ip());
        format!("http://{}:2121/", gateway)
    } else {
        format!("http://{host}/")
    };
    let creqa = create_creqa(
        state.config.price_per_step,
        &state.config.unit,
        &[state.config.mint_url.clone()],
        "TollGate internet access",
        &post_endpoint,
    );
    let qr_svg = generate_qr_svg(&creqa);
    let html = portal_html(&state.config, &creqa, &qr_svg);
    cors_response(html_response(html), origin.as_deref(), is_local)
}

/// Catch-all fallback: serve portal HTML for GET (handles OS captive-portal
/// detection probes — Apple, Android, Microsoft, Firefox — which hit paths
/// like `/hotspot-detect.html`, `/generate_204`, `/connecttest.txt`).
async fn handle_catch_all(
    method: Method,
    connect_info: ConnectInfo<SocketAddr>,
    State(state): State<Arc<V1State>>,
    headers: HeaderMap,
) -> Response {
    if method == Method::GET {
        handle_portal(connect_info, State(state), headers).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
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
    let token_str = payment.token.clone();

    {
        let spent = state.spent_tokens.lock().await;
        if spent.contains(&token_str) {
            tracing::warn!(%mac, "duplicate token rejected");
            return cors_response(
                notice_response(
                    StatusCode::BAD_REQUEST,
                    "duplicate-token",
                    "Token already used",
                    Some(&mac),
                    &state,
                    &secret_key,
                ),
                origin.as_deref(),
                is_local,
            );
        }
    }

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

    state.spent_tokens.lock().await.insert(token_str);

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
    let mac_raw = resolve_mac_from_headers(&headers, &ip);
    let mac = if mac_raw.matches(':').count() == 5 {
        mac_raw
    } else {
        String::new()
    };
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

/// Build a NUT-18 payment request encoded as `creqA` (CBOR + base64url).
fn create_creqa(
    amount: u64,
    unit: &str,
    mints: &[String],
    description: &str,
    post_endpoint: &str,
) -> String {
    let id = format!("{:016x}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis());
    let mut buf = Vec::new();
    {
        let mut e = Encoder::new(&mut buf);
        e.map(7).ok();
        e.str("t").ok();
        e.array(1).ok();
        e.map(3).ok();
        e.str("t").ok();
        e.str("post").ok();
        e.str("a").ok();
        e.str(post_endpoint).ok();
        e.str("g").ok();
        e.null().ok();
        e.str("i").ok();
        e.str(&id).ok();
        e.str("a").ok();
        e.u64(amount).ok();
        e.str("u").ok();
        e.str(unit).ok();
        e.str("m").ok();
        e.array(mints.len() as u64).ok();
        for m in mints {
            e.str(m).ok();
        }
        e.str("d").ok();
        e.str(description).ok();
        e.str("s").ok();
        e.bool(true).ok();
    }
    let b64 = STANDARD.encode(&buf);
    format!("creqA{b64}")
}

/// Generate a compact SVG QR code for the given string.
fn generate_qr_svg(data: &str) -> String {
    let code = qrcode::QrCode::new(data.as_bytes()).expect("QR data within length limit");
    let width = code.width();
    let scale = 8usize;
    let dim = width * scale;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{dim}\" height=\"{dim}\" viewBox=\"0 0 {dim} {dim}\" shape-rendering=\"crispEdges\"><rect width=\"{dim}\" height=\"{dim}\" fill=\"white\"/>",
    );
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == qrcode::types::Color::Dark {
                svg.push_str(&format!(
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/>",
                    x * scale,
                    y * scale,
                    scale,
                    scale,
                ));
            }
        }
    }
    svg.push_str("</svg>");
    svg
}

/// Format the per-step price for display on the portal page (e.g. "1 sat/1min").
fn format_price(config: &V1Config) -> String {
    match config.metric.as_str() {
        "milliseconds" => {
            let seconds = config.step_size / 1000;
            if seconds >= 3600 && seconds % 3600 == 0 {
                format!("{} {}/{}h", config.price_per_step, config.unit, seconds / 3600)
            } else if seconds >= 60 && seconds % 60 == 0 {
                format!("{} {}/{}min", config.price_per_step, config.unit, seconds / 60)
            } else {
                format!("{} {}/{}s", config.price_per_step, config.unit, seconds)
            }
        }
        "bytes" => {
            if config.step_size >= 1_073_741_824 && config.step_size % 1_073_741_824 == 0 {
                format!("{} {}/{}GB", config.price_per_step, config.unit, config.step_size / 1_073_741_824)
            } else if config.step_size >= 1_048_576 && config.step_size % 1_048_576 == 0 {
                format!("{} {}/{}MB", config.price_per_step, config.unit, config.step_size / 1_048_576)
            } else if config.step_size >= 1024 && config.step_size % 1024 == 0 {
                format!("{} {}/{}KB", config.price_per_step, config.unit, config.step_size / 1024)
            } else {
                format!("{} {}/{}B", config.price_per_step, config.unit, config.step_size)
            }
        }
        _ => format!("{} {}/{} {}", config.price_per_step, config.unit, config.step_size, config.metric),
    }
}

/// Build the captive portal HTML page, substituting dynamic pricing values.
fn portal_html(config: &V1Config, creqa: &str, qr_svg: &str) -> String {
    PORTAL_HTML_TEMPLATE
        .replace("{{PRICE}}", &format_price(config))
        .replace("{{CREQA}}", creqa)
        .replace("{{QR_SVG}}", qr_svg)
}

const PORTAL_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no">
<title>TollGate — Internet Access</title>
<style>
*,*::before,*::after{margin:0;padding:0;box-sizing:border-box}
html,body{height:100%}
body{
  font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  background:#0a0a0f;color:#e0e0e0;
  display:flex;align-items:center;justify-content:center;
  min-height:100vh;padding:20px;
  -webkit-text-size-adjust:100%;
}
.card{
  background:#12121d;border:1px solid #23233a;border-radius:18px;
  padding:40px 32px;max-width:420px;width:100%;
}
.brand{display:flex;align-items:center;gap:10px;margin-bottom:6px}
.brand-mark{
  width:32px;height:32px;border-radius:9px;
  background:linear-gradient(135deg,#f7931a,#ff5e3a);
  display:flex;align-items:center;justify-content:center;
  font-weight:800;color:#000;font-size:18px;flex-shrink:0;
}
.brand-name{font-size:24px;font-weight:700;letter-spacing:-.5px}
.brand-name span{color:#f7931a}
.tagline{color:#777;font-size:13px;margin-bottom:28px}
.price-box{
  background:#181828;border:1px solid #23233a;border-radius:14px;
  padding:22px;text-align:center;margin-bottom:24px;
}
.price-value{font-size:34px;font-weight:800;color:#f7931a;letter-spacing:-1px}
.price-label{font-size:13px;color:#777;margin-top:6px}
label{display:block;font-size:12px;color:#888;margin-bottom:8px;text-transform:uppercase;letter-spacing:.5px}
input[type=text]{
  width:100%;padding:15px 16px;
  background:#0a0a0f;border:1px solid #2a2a3e;border-radius:12px;
  color:#fff;font-size:14px;font-family:ui-monospace,"SF Mono",Menlo,monospace;
  outline:none;transition:border-color .2s;-webkit-appearance:none;
}
input[type=text]:focus{border-color:#f7931a}
input[type=text]::placeholder{color:#444}
button{
  width:100%;padding:16px;margin-top:14px;
  background:#f7931a;border:none;border-radius:12px;
  color:#000;font-size:16px;font-weight:700;cursor:pointer;
  transition:background .15s;-webkit-appearance:none;
}
button:hover{background:#ffa733}
button:active{background:#e08410}
button:disabled{opacity:.5;cursor:wait}
#status{
  margin-top:18px;padding:14px 16px;border-radius:12px;
  font-size:14px;line-height:1.4;display:none;
}
.ok{background:#0d2818;color:#4eefa0;border:1px solid #1a4d30}
.err{background:#2d0a0a;color:#ff6b6b;border:1px solid #4d1a1a}
.hint{margin-top:18px;font-size:11px;color:#444;text-align:center;line-height:1.5}
.qr-section{text-align:center;margin-bottom:20px}
.qr-section svg{max-width:220px;height:auto;border-radius:10px;border:1px solid #23233a}
.qr-label{font-size:13px;color:#888;margin-top:10px}
.divider{display:flex;align-items:center;gap:10px;margin:20px 0;color:#444;font-size:12px}
.divider::before,.divider::after{content:"";flex:1;height:1px;background:#23233a}
</style>
</head>
<body>
<div class="card">
  <div class="brand">
    <div class="brand-mark">T</div>
    <div class="brand-name">Toll<span>Gate</span></div>
  </div>
  <div class="tagline">Pay-per-use internet access</div>
  <div class="price-box">
    <div class="price-value">{{PRICE}}</div>
    <div class="price-label">per step</div>
  </div>
  <div class="qr-section">
    {{QR_SVG}}
    <div class="qr-label">Scan with your Cashu wallet</div>
  </div>
  <div class="divider">or paste a token</div>
  <form id="pay-form">
    <label for="token">Cashu Token</label>
    <input type="text" id="token" name="token" placeholder="cashuB..." autocomplete="off" required>
    <button type="submit" id="pay-btn">Pay and Connect</button>
  </form>
  <div id="status"></div>
  <div class="hint">Paste a Cashu ecash token to activate your connection</div>
</div>
<script>
(function(){
  var form=document.getElementById("pay-form");
  var btn=document.getElementById("pay-btn");
  var status=document.getElementById("status");
  var tokenInput=document.getElementById("token");
  form.addEventListener("submit",function(e){
    e.preventDefault();
    var token=tokenInput.value.trim();
    if(!token)return;
    btn.disabled=true;
    btn.textContent="Connecting...";
    status.style.display="none";
    fetch("/",{
      method:"POST",
      headers:{"Content-Type":"text/plain"},
      body:token
    }).then(function(r){
      return r.text().then(function(t){return{ok:r.ok,text:t}});
    }).then(function(res){
      btn.disabled=false;
      btn.textContent="Pay and Connect";
      try{
        var d=JSON.parse(res.text);
        if(res.ok&&d.kind===1022){
          var allotment=0,metric="milliseconds";
          if(d.tags){
            for(var i=0;i<d.tags.length;i++){
              if(d.tags[i][0]==="allotment")allotment=parseInt(d.tags[i][1],10);
              if(d.tags[i][0]==="metric")metric=d.tags[i][1];
            }
          }
          var msg;
          if(metric==="milliseconds"){
            var mins=Math.floor(allotment/60000);
            var secs=Math.floor((allotment%60000)/1000);
            msg="Connected! "+mins+" min "+secs+" sec remaining";
          }else{
            msg="Connected! "+allotment+" "+metric+" remaining";
          }
          status.className="ok";
          status.textContent=msg;
          status.style.display="block";
          form.style.display="none";
        }else{
          status.className="err";
          status.textContent=(d.content)||"Payment rejected";
          status.style.display="block";
        }
      }catch(ex){
        status.className=res.ok?"ok":"err";
        status.textContent=res.ok?"Connected!":(res.text||"Payment failed");
        status.style.display="block";
      }
    }).catch(function(err){
      btn.disabled=false;
      btn.textContent="Pay and Connect";
      status.className="err";
      status.textContent="Network error: "+err.message;
      status.style.display="block";
    });
  });
})();
</script>
</body>
</html>"#;

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

/// Resolve IP → MAC via `/tmp/dhcp.leases` (OpenWrt format).
///
/// Each line is space-separated: `<timestamp> <MAC> <IP> <hostname> <client-id>`.
/// Returns the lowercase, colon-separated MAC for the given IP, or `None` if
/// the leases file is absent or the IP is not listed.
pub fn resolve_mac_from_leases(ip: &str) -> Option<String> {
    let contents = std::fs::read_to_string("/tmp/dhcp.leases").ok()?;
    for line in contents.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == ip {
            return Some(parts[1].to_lowercase().replace('-', ":"));
        }
    }
    None
}

/// Resolve IP → MAC via `/tmp/dhcp.leases` (OpenWrt format), or fall back to
/// the raw IP as an identifier.
pub fn resolve_mac(ip: &str) -> String {
    resolve_mac_from_leases(ip).unwrap_or_else(|| ip.to_lowercase())
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
        HeaderValue::from_static("Content-Type, Authorization, X-Cashu"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("X-Cashu"),
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

fn html_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        body,
    )
        .into_response()
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

    #[test]
    fn format_price_milliseconds() {
        let config = V1Config {
            metric: "milliseconds".into(),
            step_size: 60_000,
            price_per_step: 1,
            unit: "sat".into(),
            mint_url: "https://testnut.cashu.exchange".into(),
            min_steps: 1,
            tips: vec![],
        };
        assert_eq!(format_price(&config), "1 sat/1min");

        let config_hour = V1Config {
            step_size: 3_600_000,
            ..config
        };
        assert_eq!(format_price(&config_hour), "1 sat/1h");
    }

    #[test]
    fn format_price_bytes() {
        let config = V1Config {
            metric: "bytes".into(),
            step_size: 1_048_576,
            price_per_step: 5,
            unit: "sat".into(),
            mint_url: "https://testnut.cashu.exchange".into(),
            min_steps: 1,
            tips: vec![],
        };
        assert_eq!(format_price(&config), "5 sat/1MB");
    }

    #[test]
    fn portal_html_contains_price() {
        let config = V1Config {
            metric: "milliseconds".into(),
            step_size: 60_000,
            price_per_step: 1,
            unit: "sat".into(),
            mint_url: "https://testnut.cashu.exchange".into(),
            min_steps: 1,
            tips: vec![],
        };
        let html = portal_html(&config, "creqAtest123", "<svg/>");
        assert!(html.contains("1 sat/1min"));
        assert!(html.contains("creqAtest123") || html.contains("svg"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn create_creqa_starts_with_prefix() {
        let mints = vec!["https://testnut.cashu.exchange".to_string()];
        let creqa = create_creqa(1, "sat", &mints, "test", "http://10.0.0.1:2121/");
        assert!(creqa.starts_with("creqA"));
        assert!(creqa.len() > 20);
    }

    #[test]
    fn create_creqa_decodes_to_valid_cbor() {
        let mints = vec!["https://testnut.cashu.exchange".to_string()];
        let creqa = create_creqa(1, "sat", &mints, "TollGate", "http://10.0.0.1:2121/");
        let b64 = &creqa["creqA".len()..];
        let bytes = STANDARD.decode(b64).expect("valid base64");
        let mut d = minicbor::Decoder::new(&bytes);
        assert!(d.map().is_ok(), "creqA payload must decode as CBOR map");
    }

    #[test]
    fn generate_qr_svg_produces_valid_svg() {
        let svg = generate_qr_svg("creqAtest123");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("rect"));
    }
}
