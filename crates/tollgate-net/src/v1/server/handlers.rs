#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::manual_let_else
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use nostr::prelude::*;
use tollgate_core::wallet::Wallet;

use super::merchant;
use super::{extract_client_ip, CustomerSession, LightningQuoteRecord, QuoteState, ServerState, V1ServerConfig};

pub fn build_router<W: Wallet + 'static>(state: Arc<ServerState<W>>) -> Router {
    Router::new()
        .route(
            "/",
            get(handle_get_details::<W>).post(handle_post_payment::<W>),
        )
        // `/pay` mirrors the Go captive-portal entry point. For the Cashu
        // token gate it returns the same kind:10021 advertisement (pricing)
        // the client pays against; the LN 402 + payment_request path is M-later.
        .route("/pay", get(handle_get_details::<W>))
        .route("/usage", get(handle_usage::<W>))
        .route("/whoami", get(handle_whoami::<W>))
        .route("/balance", get(handle_balance::<W>))
        .route(
            "/ln-invoice",
            get(handle_get_ln_invoice::<W>).post(handle_post_ln_invoice::<W>),
        )
        .with_state(state)
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

fn cors_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
    response
}

fn notice_response(
    level: &str,
    code: &str,
    message: &str,
    status: StatusCode,
    config: &V1ServerConfig,
) -> Response {
    match merchant::build_notice_event(level, code, message, config) {
        Ok(json) => cors_response(json_response(status, json)),
        Err(e) => {
            tracing::error!("Failed to build notice event: {e}");
            cors_response(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("internal error: {e}"),
            ))
        }
    }
}

async fn handle_get_details<W: Wallet>(State(state): State<Arc<ServerState<W>>>) -> Response {
    cors_response(json_response(StatusCode::OK, state.advertisement.clone()))
}

#[allow(clippy::too_many_lines)]
async fn handle_post_payment<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
    body: String,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);

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
            );
        }
    };

    let token = extract_payment_token(&body);

    let amount = match state.wallet.receive_token(token.as_bytes()).await {
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
            );
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let existing = state.sessions.get(&mac).await.ok().flatten();
    let session = if let Some(mut s) = existing {
        s.allotment += allotment;
        s.start_time = now;
        let updated = s.clone();
        let _ = state.sessions.update(&mac, s).await;
        updated
    } else {
        let s = CustomerSession {
            mac_address: mac.clone(),
            start_time: now,
            metric: state.config.metric.clone(),
            allotment,
        };
        let cloned = s.clone();
        let _ = state.sessions.insert(s).await;
        cloned
    };

    if let Err(e) = state.valve.open_gate(&mac).await {
        tracing::warn!("Failed to open valve for {mac}: {e}");
    }

    match merchant::build_session_event(&session, &state.config, &mac) {
        Ok(json) => cors_response(json_response(StatusCode::OK, json)),
        Err(e) => {
            tracing::error!("Failed to build session event: {e}");
            cors_response(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("internal error: {e}"),
            ))
        }
    }
}

async fn handle_usage<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => return cors_response(text_response(StatusCode::OK, "-1/-1".to_owned())),
    };

    let session = match state.sessions.get(&mac).await.ok().flatten() {
        Some(s) => s,
        None => return cors_response(text_response(StatusCode::OK, "-1/-1".to_owned())),
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
            return cors_response(text_response(StatusCode::OK, "-1/-1".to_owned()));
        }

        cors_response(text_response(
            StatusCode::OK,
            format!("{elapsed_ms}/{}", session.allotment),
        ))
    } else {
        cors_response(text_response(
            StatusCode::OK,
            format!("0/{}", session.allotment),
        ))
    }
}

async fn handle_whoami<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    match state.mac_resolver.resolve(&ip) {
        Ok(mac) => cors_response(text_response(StatusCode::OK, format!("mac={mac}"))),
        // Unknown source IP (e.g. request not from a DHCP client): return an
        // empty value rather than a bogus MAC so callers can detect "unknown".
        Err(_) => cors_response(text_response(StatusCode::OK, "mac=".to_owned())),
    }
}

async fn handle_balance<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => {
            return cors_response(json_response(
                StatusCode::OK,
                r#"{"status":1,"session_active":false}"#.to_owned(),
            ));
        }
    };

    let session = match state.sessions.get(&mac).await.ok().flatten() {
        Some(s) => s,
        None => {
            return cors_response(json_response(
                StatusCode::OK,
                r#"{"status":1,"session_active":false}"#.to_owned(),
            ));
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
            return cors_response(json_response(
                StatusCode::OK,
                r#"{"status":1,"session_active":false}"#.to_owned(),
            ));
        }

        let rem = session.allotment as i64 - elapsed_ms;
        (elapsed_ms as u64, rem.max(0) as u64)
    } else {
        (0, session.allotment)
    };

    let json = serde_json::json!({
        "status": 1,
        "session_active": true,
        "metric": session.metric,
        "usage": usage,
        "allotment": session.allotment,
        "remaining": remaining,
        "start_time": session.start_time,
    });

    cors_response(json_response(
        StatusCode::OK,
        serde_json::to_string(&json).unwrap_or_default(),
    ))
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
/// - `payment-error-token-spent` — token already redeemed
/// - `payment-error-invalid-token` — malformed or unparseable token
/// - `payment-processing-failed` — generic payment failure
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
    #[serde(skip_serializing_if = "Option::is_none")]
    quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invoice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mint_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<u64>,
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
        quote: None,
        invoice: None,
        mint_url: None,
        amount: None,
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
async fn handle_post_ln_invoice<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
    body: String,
) -> Response {
    let req: LnInvoiceRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => {
            return cors_response(ln_error_response(
                StatusCode::BAD_REQUEST,
                "amount and mint_url are required",
            ));
        }
    };

    let mint_url = req.mint_url.or(req.mint).unwrap_or_default();

    if req.amount == 0 || mint_url.is_empty() {
        return cors_response(ln_error_response(
            StatusCode::BAD_REQUEST,
            "amount and mint_url are required",
        ));
    }

    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(e) => {
            tracing::warn!("MAC resolution failed for {ip}: {e}");
            return cors_response(ln_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot resolve MAC address",
            ));
        }
    };

    if !state
        .config
        .accepted_mints
        .iter()
        .any(|m| m.url == mint_url)
    {
        return cors_response(ln_error_response(
            StatusCode::BAD_REQUEST,
            "mint not accepted",
        ));
    }

    let wallet = match &state.mint_quote_wallet {
        Some(w) => Arc::clone(w),
        None => {
            return cors_response(ln_error_response(
                StatusCode::BAD_REQUEST,
                "lightning payments not available",
            ));
        }
    };

    let info = match wallet.request_mint_quote(req.amount, &mint_url).await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("Mint quote request failed: {e}");
            return cors_response(ln_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("quote request failed: {e}"),
            ));
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
        return cors_response(ln_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error",
        ));
    }

    let resp = LnInvoiceResponse {
        status: 1,
        quote: Some(info.quote_id),
        invoice: Some(info.invoice),
        mint_url: Some(mint_url),
        amount: Some(info.amount),
        expiry: Some(info.expiry),
        state: "UNPAID".to_owned(),
        access_granted: false,
        allotment: None,
        metric: None,
        error: None,
    };

    cors_response(json_response(
        StatusCode::OK,
        serde_json::to_string(&resp).unwrap_or_default(),
    ))
}

#[allow(clippy::too_many_lines)]
async fn handle_get_ln_invoice<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<Arc<ServerState<W>>>,
    Query(params): Query<LnInvoiceQuery>,
) -> Response {
    let quote_id = match params.quote {
        Some(q) if !q.is_empty() => q,
        _ => {
            return cors_response(ln_error_response(
                StatusCode::BAD_REQUEST,
                "quote is required",
            ));
        }
    };

    let ip = extract_client_ip(Some(&ConnectInfo(addr)), &headers);
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(e) => {
            tracing::warn!("MAC resolution failed for {ip}: {e}");
            // Return 200 with an error body: busybox wget (used by clients and
            // the test harness) discards the body on non-2xx responses.
            return cors_response(ln_error_response(
                StatusCode::OK,
                "cannot resolve MAC address",
            ));
        }
    };

    let mut record = match state.lightning_quotes.get_for_mac(&quote_id, &mac).await {
        Ok(Some(r)) => r,
        Ok(None) | Err(_) => {
            return cors_response(ln_error_response(StatusCode::OK, "quote not found"));
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
                    return cors_response(ln_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "mint tokens failed",
                    ));
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
                    return cors_response(ln_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "allotment calculation failed",
                    ));
                }
            };

        let existing = state.sessions.get(&mac).await.ok().flatten();
        if let Some(mut s) = existing {
            s.allotment += allotment;
            s.start_time = now;
            let _ = state.sessions.update(&mac, s).await;
        } else {
            let s = CustomerSession {
                mac_address: mac.clone(),
                start_time: now,
                metric: state.config.metric.clone(),
                allotment,
            };
            let _ = state.sessions.insert(s).await;
        }

        if let Err(e) = state.valve.open_gate(&mac).await {
            tracing::warn!("Failed to open valve for {mac}: {e}");
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
            quote: Some(record.quote_id),
            invoice: None,
            mint_url: Some(record.mint_url),
            amount: Some(record.amount),
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
            quote: Some(record.quote_id),
            invoice: None,
            mint_url: Some(record.mint_url),
            amount: Some(record.amount),
            expiry: None,
            state: state_str.to_owned(),
            access_granted: false,
            allotment: None,
            metric: None,
            error: None,
        }
    };

    cors_response(json_response(
        StatusCode::OK,
        serde_json::to_string(&resp).unwrap_or_default(),
    ))
}
