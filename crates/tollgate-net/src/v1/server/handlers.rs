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

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use nostr::prelude::*;
use tollgate_core::wallet::Wallet;

use super::merchant;
use super::{CustomerSession, ServerState, V1ServerConfig};

pub fn build_router<W: Wallet + 'static>(state: Arc<ServerState<W>>) -> Router {
    Router::new()
        .route(
            "/",
            get(handle_get_details::<W>).post(handle_post_payment::<W>),
        )
        .route("/usage", get(handle_usage::<W>))
        .route("/whoami", get(handle_whoami::<W>))
        .route("/balance", get(handle_balance::<W>))
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
    State(state): State<Arc<ServerState<W>>>,
    body: String,
) -> Response {
    let ip = addr.ip().to_string();

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
            return notice_response(
                "error",
                "token_rejected",
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
                "insufficient_payment",
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

    let session = {
        let mut sessions = state.sessions.lock().await;
        sessions
            .entry(mac.clone())
            .and_modify(|s| {
                s.allotment += allotment;
                s.start_time = now;
            })
            .or_insert_with(|| CustomerSession {
                mac_address: mac.clone(),
                start_time: now,
                metric: state.config.metric.clone(),
                allotment,
            });
        sessions.get(&mac).cloned().unwrap()
    };

    if let Err(e) = state.valve.open_gate(&mac) {
        tracing::warn!("Failed to open valve for {mac}: {e}");
    }

    match merchant::build_session_event(&session, &state.config) {
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
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = addr.ip().to_string();
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => return cors_response(text_response(StatusCode::OK, "-1/-1".to_owned())),
    };

    let mut sessions = state.sessions.lock().await;
    let session = match sessions.get(&mac) {
        Some(s) => s.clone(),
        None => return cors_response(text_response(StatusCode::OK, "-1/-1".to_owned())),
    };

    if session.metric == "milliseconds" {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let elapsed_ms = (now - session.start_time) * 1000;

        if elapsed_ms >= session.allotment as i64 {
            sessions.remove(&mac);
            if let Err(e) = state.valve.close_gate(&mac) {
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
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = addr.ip().to_string();
    match state.mac_resolver.resolve(&ip) {
        Ok(mac) => cors_response(text_response(StatusCode::OK, format!("mac={mac}"))),
        Err(_) => cors_response(text_response(StatusCode::OK, "mac=unknown".to_owned())),
    }
}

async fn handle_balance<W: Wallet>(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<ServerState<W>>>,
) -> Response {
    let ip = addr.ip().to_string();
    let mac = match state.mac_resolver.resolve(&ip) {
        Ok(mac) => mac,
        Err(_) => {
            return cors_response(json_response(
                StatusCode::OK,
                r#"{"status":1,"session_active":false}"#.to_owned(),
            ));
        }
    };

    let mut sessions = state.sessions.lock().await;
    let session = match sessions.get(&mac) {
        Some(s) => s.clone(),
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
            sessions.remove(&mac);
            if let Err(e) = state.valve.close_gate(&mac) {
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
