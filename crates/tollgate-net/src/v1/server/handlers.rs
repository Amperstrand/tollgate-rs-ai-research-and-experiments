#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::manual_let_else
)]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use nostr::prelude::*;

use super::merchant;
use super::{extract_client_ip, LightningQuoteRecord, QuoteState, ServerState, V1ServerConfig};

pub fn build_router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route(
            "/",
            get(handle_get_details)
                .post(handle_post_payment)
                .options(handle_options),
        )
        .route("/pay", get(handle_get_details).options(handle_options))
        .route("/usage", get(handle_usage).options(handle_options))
        .route("/whoami", get(handle_whoami).options(handle_options))
        .route("/balance", get(handle_balance).options(handle_options))
        .route(
            "/ln-invoice",
            get(handle_get_ln_invoice)
                .post(handle_post_ln_invoice)
                .options(handle_options),
        )
        .with_state(state)
        // Go v1 parity: cap request body at 1MB via http.MaxBytesReader(w, r.Body, 1<<20)
        .layer(DefaultBodyLimit::max(1_048_576))
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

fn text_response(status: StatusCode, body: String) -> Response {
    (status, body).into_response()
}

fn get_origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
}

/// Check whether the given Origin URL refers to a local/private origin.
/// Mirrors Go v1's `isLocalOrigin`: checks IP against private/loopback
/// ranges, "localhost" hostname, and DNS-resolved IPs.
async fn is_local_origin(origin: &str) -> bool {
    let Ok(parsed) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback() || matches!(v6.octets(), [0xfd, ..]),
        };
    }

    if host == "localhost" {
        return true;
    }

    let lookup_target = format!("{host}:0");
    if let Ok(addrs) = tokio::net::lookup_host(&lookup_target).await {
        for addr in addrs {
            let ip = addr.ip();
            let private = match ip {
                IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
                IpAddr::V6(v6) => v6.is_loopback() || matches!(v6.octets(), [0xfd, ..]),
            };
            if private {
                return true;
            }
        }
    }

    false
}

fn cors_response(mut response: Response, origin: Option<&str>, is_local: bool) -> Response {
    let headers = response.headers_mut();
    if let Some(origin_val) = origin {
        if is_local && !origin_val.is_empty() {
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

fn notice_response(
    level: &str,
    code: &str,
    message: &str,
    status: StatusCode,
    config: &V1ServerConfig,
    origin: Option<&str>,
    is_local: bool,
) -> Response {
    match merchant::build_notice_event(level, code, message, config) {
        Ok(json) => cors_response(json_response(status, json), origin, is_local),
        Err(e) => {
            tracing::error!("Failed to build notice event: {e}");
            cors_response(
                text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("internal error: {e}"),
                ),
                origin,
                is_local,
            )
        }
    }
}

async fn resolve_origin(headers: &HeaderMap) -> (Option<String>, bool) {
    let origin = get_origin(headers);
    let is_local = match origin.as_deref() {
        Some(o) => is_local_origin(o).await,
        None => false,
    };
    (origin, is_local)
}

async fn handle_get_details(headers: HeaderMap, State(state): State<Arc<ServerState>>) -> Response {
    let (origin, is_local) = resolve_origin(&headers).await;
    cors_response(
        json_response(StatusCode::OK, state.advertisement.clone()),
        origin.as_deref(),
        is_local,
    )
}

async fn handle_options(headers: HeaderMap) -> Response {
    let (origin, is_local) = resolve_origin(&headers).await;
    cors_response(StatusCode::OK.into_response(), origin.as_deref(), is_local)
}

async fn open_gate_for_session(
    valve: &dyn super::Valve,
    mac: &str,
    metric: &str,
    start_time: i64,
    allotment: u64,
) -> Result<(), super::ValveError> {
    if metric == "milliseconds" {
        let until_ts = start_time + (allotment as i64 / 1000);
        valve.open_gate_until(mac, until_ts).await?;
    } else {
        valve.open_gate(mac).await?;
    }
    Ok(())
}

async fn rollback_session(
    sessions: &dyn super::SessionStore,
    mac: &str,
    prior: Option<super::CustomerSession>,
) {
    match prior {
        Some(old) => {
            if let Err(e) = sessions.update(mac, old).await {
                tracing::error!("Failed to restore prior session for {mac}: {e}");
            }
        }
        None => {
            if let Err(e) = sessions.remove(mac).await {
                tracing::error!("Failed to remove new session for {mac}: {e}");
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_post_payment(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
    body: String,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers).await;

    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(e) => {
            tracing::warn!("MAC resolution failed for {ip}: {e}");
            return notice_response(
                "error",
                "mac_resolution_failed",
                &format!("cannot resolve MAC for {ip}"),
                StatusCode::INTERNAL_SERVER_ERROR,
                &state.config,
                origin.as_deref(),
                is_local,
            );
        }
    };

    let token = extract_payment_token(&body);

    let amount = match state.merchant.get().receive_token(token.as_bytes()).await {
        Ok(amount) => amount,
        Err(e) => {
            tracing::warn!("Token rejected: {e}");
            let code = classify_payment_error(&e.to_string());
            return notice_response(
                "error",
                code,
                &format!("payment rejected: {e}"),
                StatusCode::BAD_REQUEST,
                &state.config,
                origin.as_deref(),
                is_local,
            );
        }
    };

    let mint_url = state
        .config
        .accepted_mints
        .first()
        .map(|m| m.url.clone())
        .unwrap_or_default();

    let allotment = match merchant::calculate_allotment(amount.0, &mint_url, &state.config) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Allotment calculation failed: {e}");
            return notice_response(
                "error",
                "allotment-calculation-failed",
                &format!("{e}"),
                StatusCode::BAD_REQUEST,
                &state.config,
                origin.as_deref(),
                is_local,
            );
        }
    };

    let prior_session = state.sessions.get(&mac).await.ok().flatten();

    let session = match super::merchant_provider::add_allotment(
        &*state.sessions,
        &mac,
        &state.config.metric,
        allotment,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Session upsert failed: {e}");
            return notice_response(
                "error",
                "session-error",
                &format!("failed to update session: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
                &state.config,
                origin.as_deref(),
                is_local,
            );
        }
    };

    if let Err(e) = open_gate_for_session(
        &*state.valve,
        &mac,
        &session.metric,
        session.start_time,
        session.allotment,
    )
    .await
    {
        tracing::error!("Valve open failed for {mac}, rolling back session: {e}");
        rollback_session(&*state.sessions, &mac, prior_session).await;
        return notice_response(
            "error",
            "valve-error",
            &format!("valve open failed, session rolled back: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
            &state.config,
            origin.as_deref(),
            is_local,
        );
    }

    match merchant::build_session_event(&session, &state.config, &mac) {
        Ok(json) => cors_response(
            json_response(StatusCode::OK, json),
            origin.as_deref(),
            is_local,
        ),
        Err(e) => {
            tracing::error!("Failed to build session event: {e}");
            cors_response(
                text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("internal error: {e}"),
                ),
                origin.as_deref(),
                is_local,
            )
        }
    }
}

async fn handle_usage(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers).await;

    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => {
            return cors_response(
                text_response(StatusCode::OK, "-1/-1".to_owned()),
                origin.as_deref(),
                is_local,
            )
        }
    };

    let session = match state.sessions.get(&mac).await.ok().flatten() {
        Some(s) => s,
        None => {
            return cors_response(
                text_response(StatusCode::OK, "-1/-1".to_owned()),
                origin.as_deref(),
                is_local,
            )
        }
    };

    if session.metric == "milliseconds" {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let elapsed_ms = (now - session.start_time) * 1000;

        if elapsed_ms >= session.allotment as i64 {
            let _ = state.sessions.remove(&mac).await;
            if let Err(e) = state.valve.close_gate(&mac).await {
                tracing::warn!("Failed to close valve for {mac}: {e}");
            }
            return cors_response(
                text_response(StatusCode::OK, "-1/-1".to_owned()),
                origin.as_deref(),
                is_local,
            );
        }

        cors_response(
            text_response(
                StatusCode::OK,
                format!("{elapsed_ms}/{}", session.allotment),
            ),
            origin.as_deref(),
            is_local,
        )
    } else {
        let usage = state
            .valve
            .get_client_usage_since_baseline(&mac)
            .await
            .unwrap_or(0);
        if usage >= session.allotment {
            let _ = state.sessions.remove(&mac).await;
            if let Err(e) = state.valve.close_gate(&mac).await {
                tracing::warn!("Failed to close valve for {mac}: {e}");
            }
            return cors_response(
                text_response(StatusCode::OK, "-1/-1".to_owned()),
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

async fn handle_whoami(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers).await;
    match state.mac_resolver.resolve(&ip) {
        Ok(mac) => cors_response(
            text_response(StatusCode::OK, format!("mac={mac}")),
            origin.as_deref(),
            is_local,
        ),
        Err(_) => cors_response(
            text_response(StatusCode::OK, "mac=".to_owned()),
            origin.as_deref(),
            is_local,
        ),
    }
}

async fn handle_balance(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let (origin, is_local) = resolve_origin(&headers).await;
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => {
            return cors_response(
                json_response(
                    StatusCode::OK,
                    r#"{"status":1,"session_active":false}"#.to_owned(),
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let session = match state.sessions.get(&mac).await.ok().flatten() {
        Some(s) => s,
        None => {
            return cors_response(
                json_response(
                    StatusCode::OK,
                    r#"{"status":1,"session_active":false}"#.to_owned(),
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let (usage, remaining) = if session.metric == "milliseconds" {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let elapsed_ms = (now - session.start_time) * 1000;

        if elapsed_ms >= session.allotment as i64 {
            let _ = state.sessions.remove(&mac).await;
            if let Err(e) = state.valve.close_gate(&mac).await {
                tracing::warn!("Failed to close valve for {mac}: {e}");
            }
            return cors_response(
                json_response(
                    StatusCode::OK,
                    r#"{"status":1,"session_active":false}"#.to_owned(),
                ),
                origin.as_deref(),
                is_local,
            );
        }

        let rem = session.allotment as i64 - elapsed_ms;
        (elapsed_ms as u64, rem.max(0) as u64)
    } else {
        let usage = state
            .valve
            .get_client_usage_since_baseline(&mac)
            .await
            .unwrap_or(0);
        if usage >= session.allotment {
            let _ = state.sessions.remove(&mac).await;
            if let Err(e) = state.valve.close_gate(&mac).await {
                tracing::warn!("Failed to close valve for {mac}: {e}");
            }
            return cors_response(
                json_response(
                    StatusCode::OK,
                    r#"{"status":1,"session_active":false}"#.to_owned(),
                ),
                origin.as_deref(),
                is_local,
            );
        }
        let rem = session.allotment.saturating_sub(usage);
        (usage, rem)
    };

    let mut json = serde_json::json!({
        "status": 1,
        "session_active": true,
        "usage": usage,
        "allotment": session.allotment,
        "remaining": remaining,
        "start_time": session.start_time,
    });

    if !session.metric.is_empty() {
        json.as_object_mut()
            .unwrap()
            .insert("metric".to_owned(), serde_json::Value::String(session.metric.clone()));
    }

    cors_response(
        json_response(
            StatusCode::OK,
            serde_json::to_string(&json).unwrap_or_default(),
        ),
        origin.as_deref(),
        is_local,
    )
}

fn extract_payment_token(body: &str) -> String {
    if let Ok(event) = Event::from_json(body) {
        if event.kind == Kind::Custom(21_000) {
            for tag in event.tags.iter() {
                let items = tag.as_slice();
                if items.first().map(String::as_str) == Some("payment") {
                    if let Some(token) = items.get(1) {
                        return token.clone();
                    }
                }
            }
        }
    }
    body.to_owned()
}

/// Classify a wallet error into a Go v1-compatible error code string.
///
/// Go v1 uses specific error codes:
/// - `payment-error-token-spent` -- token already redeemed
/// - `payment-error-invalid-token` -- malformed or unparseable token
/// - `payment-processing-failed` -- generic payment failure
fn classify_payment_error(err_str: &str) -> &'static str {
    let lower = err_str.to_ascii_lowercase();
    if lower.contains("already spent") || lower.contains("token already") {
        "payment-error-token-spent"
    } else if lower.contains("invalid token")
        || lower.contains("decode")
        || lower.contains("token rejected")
        || lower.contains("too short")
        || lower.contains("zero amount")
    {
        "payment-error-invalid-token"
    } else {
        "payment-processing-failed"
    }
}

#[derive(serde::Deserialize)]
struct LnInvoiceRequest {
    amount: u64,
    mint_url: Option<String>,
    mint: Option<String>,
}

#[derive(serde::Serialize)]
struct LnInvoiceResponse {
    status: u8,
    quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    invoice: Option<String>,
    mint_url: String,
    amount: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    expiry: Option<u64>,
    state: String,
    access_granted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    allotment: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metric: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct LnInvoiceQuery {
    quote: Option<String>,
}

fn ln_error_response(http_status: StatusCode, error: &str) -> Response {
    let resp = LnInvoiceResponse {
        status: 0,
        quote: String::new(),
        invoice: None,
        mint_url: String::new(),
        amount: 0,
        expiry: None,
        state: String::new(),
        access_granted: false,
        allotment: None,
        metric: None,
        error: Some(error.to_owned()),
    };
    json_response(
        http_status,
        serde_json::to_string(&resp).unwrap_or_default(),
    )
}

#[allow(clippy::too_many_lines)]
async fn handle_post_ln_invoice(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
    body: String,
) -> Response {
    let (origin, is_local) = resolve_origin(&headers).await;

    let req: LnInvoiceRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => {
            return cors_response(
                ln_error_response(StatusCode::BAD_REQUEST, "amount and mint_url are required"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let mint_url = req.mint_url.or(req.mint).unwrap_or_default();

    if req.amount == 0 || mint_url.is_empty() {
        return cors_response(
            ln_error_response(StatusCode::BAD_REQUEST, "amount and mint_url are required"),
            origin.as_deref(),
            is_local,
        );
    }

    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(e) => {
            tracing::warn!("MAC resolution failed for {ip}: {e}");
            return cors_response(
                ln_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cannot resolve MAC address",
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    if !state
        .config
        .accepted_mints
        .iter()
        .any(|m| m.url == mint_url)
    {
        return cors_response(
            ln_error_response(StatusCode::BAD_REQUEST, "mint not accepted"),
            origin.as_deref(),
            is_local,
        );
    }

    let wallet = match &state.mint_quote_wallet {
        Some(w) => Arc::clone(w),
        None => {
            return cors_response(
                ln_error_response(StatusCode::BAD_REQUEST, "lightning payments not available"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let info = match wallet.request_mint_quote(req.amount, &mint_url).await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("Mint quote request failed: {e}");
            return cors_response(
                ln_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("quote request failed: {e}"),
                ),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let record = LightningQuoteRecord {
        quote_id: info.quote_id.clone(),
        mac_address: mac,
        mint_url: mint_url.clone(),
        amount: info.amount,
        expiry: info.expiry as i64,
        allotment: 0,
        created_at: now,
        completed_at: None,
        session_granted: false,
        processing: false,
        invoice: info.invoice.clone(),
        cached_state: Some(QuoteState::Unpaid),
        cached_state_at: Some(now),
    };

    if let Err(e) = state.lightning_quotes.insert(record).await {
        tracing::error!("Failed to store lightning quote: {e}");
        return cors_response(
            ln_error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
            origin.as_deref(),
            is_local,
        );
    }

    let resp = LnInvoiceResponse {
        status: 1,
        quote: info.quote_id,
        invoice: Some(info.invoice),
        mint_url,
        amount: info.amount,
        expiry: Some(info.expiry),
        state: "UNPAID".to_owned(),
        access_granted: false,
        allotment: None,
        metric: None,
        error: None,
    };

    cors_response(
        json_response(
            StatusCode::OK,
            serde_json::to_string(&resp).unwrap_or_default(),
        ),
        origin.as_deref(),
        is_local,
    )
}

#[allow(clippy::too_many_lines)]
async fn handle_get_ln_invoice(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
    Query(params): Query<LnInvoiceQuery>,
) -> Response {
    let (origin, is_local) = resolve_origin(&headers).await;

    let quote_id = match params.quote {
        Some(q) if !q.is_empty() => q,
        _ => {
            return cors_response(
                ln_error_response(StatusCode::BAD_REQUEST, "quote is required"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(e) => {
            tracing::warn!("MAC resolution failed for {ip}: {e}");
            return cors_response(
                ln_error_response(StatusCode::OK, "cannot resolve MAC address"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let mut record = match state.lightning_quotes.get_for_mac(&quote_id, &mac).await {
        Ok(Some(r)) => r,
        Ok(None) | Err(_) => {
            return cors_response(
                ln_error_response(StatusCode::OK, "quote not found"),
                origin.as_deref(),
                is_local,
            );
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let cache_age = record.cached_state_at.map_or(i64::MAX, |at| now - at);

    if cache_age >= 2 {
        if let Some(ref wallet) = state.mint_quote_wallet {
            match wallet.check_mint_quote_status(&quote_id).await {
                Ok(s) => {
                    record.cached_state = Some(s);
                    record.cached_state_at = Some(now);
                    let _ = state
                        .lightning_quotes
                        .update(&quote_id, record.clone())
                        .await;
                }
                Err(e) => {
                    tracing::warn!("Failed to check quote status: {e}");
                }
            }
        }
    }

    let cached_state = record.cached_state.unwrap_or(QuoteState::Unpaid);

    if (cached_state == QuoteState::Paid || cached_state == QuoteState::Issued)
        && !record.session_granted
        && !record.processing
    {
        record.processing = true;
        let _ = state
            .lightning_quotes
            .update(&quote_id, record.clone())
            .await;

        if cached_state == QuoteState::Paid {
            if let Some(ref wallet) = state.mint_quote_wallet {
                if let Err(e) = wallet.mint_tokens(&quote_id).await {
                    tracing::error!("Mint tokens failed: {e}");
                    record.processing = false;
                    let _ = state
                        .lightning_quotes
                        .update(&quote_id, record.clone())
                        .await;
                    return cors_response(
                        ln_error_response(StatusCode::INTERNAL_SERVER_ERROR, "mint tokens failed"),
                        origin.as_deref(),
                        is_local,
                    );
                }
            }
        }

        let allotment =
            match merchant::calculate_allotment(record.amount, &record.mint_url, &state.config) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Allotment calculation failed: {e}");
                    record.processing = false;
                    let _ = state
                        .lightning_quotes
                        .update(&quote_id, record.clone())
                        .await;
                    return cors_response(
                        ln_error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "allotment calculation failed",
                        ),
                        origin.as_deref(),
                        is_local,
                    );
                }
            };

        let prior_session = state.sessions.get(&mac).await.ok().flatten();

        let session = match super::merchant_provider::add_allotment(
            &*state.sessions,
            &mac,
            &state.config.metric,
            allotment,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Session upsert failed for LN invoice: {e}");
                record.processing = false;
                let _ = state
                    .lightning_quotes
                    .update(&quote_id, record.clone())
                    .await;
                return cors_response(
                    ln_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to update session",
                    ),
                    origin.as_deref(),
                    is_local,
                );
            }
        };

        let effective_allotment = session.allotment;

        if let Err(e) = open_gate_for_session(
            &*state.valve,
            &mac,
            &state.config.metric,
            now,
            effective_allotment,
        )
        .await
        {
            tracing::error!("Valve open failed for LN session {mac}, rolling back: {e}");
            rollback_session(&*state.sessions, &mac, prior_session).await;
            record.processing = false;
            let _ = state
                .lightning_quotes
                .update(&quote_id, record.clone())
                .await;
            return cors_response(
                ln_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "valve open failed, session rolled back",
                ),
                origin.as_deref(),
                is_local,
            );
        }

        record.session_granted = true;
        record.completed_at = Some(now);
        record.allotment = allotment;
        record.cached_state = Some(QuoteState::Issued);
        let _ = state
            .lightning_quotes
            .update(&quote_id, record.clone())
            .await;
    }

    let resp = if record.session_granted {
        LnInvoiceResponse {
            status: 1,
            quote: record.quote_id,
            invoice: None,
            mint_url: record.mint_url,
            amount: record.amount,
            expiry: None,
            state: "ISSUED".to_owned(),
            access_granted: true,
            allotment: Some(record.allotment),
            metric: Some(state.config.metric.clone()),
            error: None,
        }
    } else {
        let state_str = match record.cached_state {
            Some(QuoteState::Paid) => "PAID",
            Some(QuoteState::Issued) => "ISSUED",
            _ => "UNPAID",
        };
        LnInvoiceResponse {
            status: 1,
            quote: record.quote_id,
            invoice: None,
            mint_url: record.mint_url,
            amount: record.amount,
            expiry: None,
            state: state_str.to_owned(),
            access_granted: false,
            allotment: None,
            metric: None,
            error: None,
        }
    };

    cors_response(
        json_response(
            StatusCode::OK,
            serde_json::to_string(&resp).unwrap_or_default(),
        ),
        origin.as_deref(),
        is_local,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_is_local_origin_private_ipv4() {
        assert!(is_local_origin("http://192.168.1.1").await);
        assert!(is_local_origin("http://192.168.1.1/path").await);
        assert!(is_local_origin("http://10.0.0.1").await);
        assert!(is_local_origin("http://10.255.255.255").await);
        assert!(is_local_origin("http://172.16.0.1").await);
        assert!(is_local_origin("http://172.31.255.255").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_loopback() {
        assert!(is_local_origin("http://127.0.0.1:8080").await);
        assert!(is_local_origin("http://127.0.0.1").await);
        assert!(is_local_origin("http://127.255.255.255").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_localhost() {
        assert!(is_local_origin("http://localhost:8080").await);
        assert!(is_local_origin("http://localhost").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_ipv6_loopback() {
        assert!(is_local_origin("http://[::1]:8080").await);
        assert!(is_local_origin("http://[::1]").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_public_ips() {
        assert!(!is_local_origin("http://8.8.8.8").await);
        assert!(!is_local_origin("http://1.1.1.1").await);
        assert!(!is_local_origin("http://203.0.113.1").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_public_hostnames() {
        assert!(!is_local_origin("http://example.com").await);
        assert!(!is_local_origin("https://example.com").await);
    }

    #[tokio::test]
    async fn test_is_local_origin_invalid() {
        assert!(!is_local_origin("").await);
        assert!(!is_local_origin("not-a-url").await);
        assert!(!is_local_origin("://missing-scheme").await);
    }

    #[tokio::test]
    async fn test_cors_response_with_local_origin() {
        let resp = text_response(StatusCode::OK, "test".to_owned());
        let resp = cors_response(resp, Some("http://192.168.1.1:8080"), true);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("http://192.168.1.1:8080")
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-methods")
                .map(|v| v.to_str().unwrap()),
            Some("GET, POST, OPTIONS")
        );
        assert_eq!(
            resp.headers()
                .get("access-control-allow-headers")
                .map(|v| v.to_str().unwrap()),
            Some("Content-Type, Authorization")
        );
    }

    #[tokio::test]
    async fn test_cors_response_with_public_origin() {
        let resp = text_response(StatusCode::OK, "test".to_owned());
        let resp = cors_response(resp, Some("http://example.com"), false);
        assert!(resp.headers().get("access-control-allow-origin").is_none());
        assert!(resp.headers().get("access-control-allow-methods").is_some());
        assert!(resp.headers().get("access-control-allow-headers").is_some());
    }

    #[tokio::test]
    async fn test_cors_response_no_origin() {
        let resp = text_response(StatusCode::OK, "test".to_owned());
        let resp = cors_response(resp, None, false);
        assert!(resp.headers().get("access-control-allow-origin").is_none());
        assert!(resp.headers().get("access-control-allow-methods").is_some());
        assert!(resp.headers().get("access-control-allow-headers").is_some());
    }
}
