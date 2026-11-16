//! REST API for external session management (tollgate-auth consumer).
//!
//! Provides api-key-gated endpoints for querying sessions, reporting
//! external usage, and managing allotments. This is the REST surface
//! that tollgate-auth's Delegated Mode uses.
//!
//! Authentication: `X-API-Key` header checked against the
//! `TOLLGATE_API_KEY` environment variable.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use super::merchant::{CustomerSession, ExternalUsageSnapshot};
use super::session_store::{SessionStore, now_secs};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SessionApiState {
    pub sessions: Arc<dyn SessionStore>,
    pub metric: String,
}

// ---------------------------------------------------------------------------
// Request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BootstrapRequest {
    pub access_method: String,
    pub token: String,
    pub mac: String,
    #[serde(default)]
    pub metric: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub metric: String,
    pub allotment: u64,
    pub usage: u64,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_external_usage: Option<ExternalUsageSnapshot>,
}

#[derive(Debug, Deserialize)]
pub struct UsageReport {
    #[serde(default)]
    pub source: Option<String>,
    pub input_octets: Option<u64>,
    pub output_octets: Option<u64>,
    pub session_time: Option<u64>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TopupRequest {
    pub token: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn check_api_key(headers: &HeaderMap) -> bool {
    match std::env::var("TOLLGATE_API_KEY") {
        Ok(key) => headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|k| k == key)
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn error_json(status: StatusCode, error: &str, session_id: Option<&str>) -> Response {
    let mut body = serde_json::json!({"error": error});
    if let Some(id) = session_id {
        body["session_id"] = serde_json::json!(id);
    }
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_default(),
    )
        .into_response()
}

fn json_ok<T: Serialize>(value: &T) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(value).unwrap_or_default(),
    )
        .into_response()
}

fn normalize_mac(mac: &str) -> String {
    mac.to_lowercase().replace(['-', '.'], ":")
}

fn build_session_response(session: &CustomerSession) -> SessionResponse {
    let elapsed = now_secs() - session.start_time;
    let usage = if session.metric == "milliseconds" {
        (elapsed * 1000).min(session.allotment as i64).max(0) as u64
    } else {
        0
    };
    let active = usage < session.allotment;

    SessionResponse {
        session_id: session.mac_address.clone(),
        metric: session.metric.clone(),
        allotment: session.allotment,
        usage,
        active,
        last_external_usage: session.last_external_usage.clone(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn handle_get_session(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
) -> Response {
    if !check_api_key(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&mac);

    match state.sessions.get(&mac).await {
        Ok(Some(session)) => {
            let resp = build_session_response(&session);
            json_ok(&resp)
        }
        Ok(None) => error_json(StatusCode::NOT_FOUND, "session not found", Some(&mac)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"session store error: {e}\"}}"),
        )
            .into_response(),
    }
}

async fn handle_post_usage(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
    Json(report): Json<UsageReport>,
) -> Response {
    if !check_api_key(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&mac);

    let snapshot = ExternalUsageSnapshot {
        input_octets: report.input_octets.unwrap_or(0),
        output_octets: report.output_octets.unwrap_or(0),
        session_time: report.session_time.unwrap_or(0),
        reported_at: now_secs(),
    };

    match state.sessions.record_external_usage(&mac, snapshot).await {
        Ok(Some(updated)) => {
            let resp = build_session_response(&updated);
            json_ok(&resp)
        }
        Ok(None) => error_json(StatusCode::NOT_FOUND, "session not found", Some(&mac)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"session store error: {e}\"}}"),
        )
            .into_response(),
    }
}

async fn handle_post_topup(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
    Json(_req): Json<TopupRequest>,
) -> Response {
    if !check_api_key(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&mac);

    // Token redemption for top-up requires the wallet — deferred to
    // integration with the payment path. For now, return a clear error.
    let _ = &state;
    error_json(
        StatusCode::NOT_IMPLEMENTED,
        "topup via session API requires wallet integration (not yet wired)",
        Some(&mac),
    )
}

async fn handle_delete_session(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
) -> Response {
    if !check_api_key(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&mac);

    match state.sessions.remove(&mac).await {
        Ok(Some(_)) => (
            StatusCode::OK,
            format!("{{\"deleted\":true,\"session_id\":\"{mac}\"}}"),
        )
            .into_response(),
        Ok(None) => error_json(StatusCode::NOT_FOUND, "session not found", Some(&mac)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{{\"error\":\"session store error: {e}\"}}"),
        )
            .into_response(),
    }
}

async fn handle_post_bootstrap(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Json(req): Json<BootstrapRequest>,
) -> Response {
    if !check_api_key(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&req.mac);

    // Token redemption requires the wallet — deferred to integration
    // with the payment path. For now, return a clear error.
    let _ = (&req, &state);
    error_json(
        StatusCode::NOT_IMPLEMENTED,
        "bootstrap via session API requires wallet integration (not yet wired)",
        Some(&mac),
    )
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn build_session_router(state: Arc<SessionApiState>) -> axum::Router {
    axum::Router::new()
        .route(
            "/sessions/{mac}",
            get(handle_get_session).delete(handle_delete_session),
        )
        .route("/sessions/{mac}/usage", post(handle_post_usage))
        .route("/sessions/{mac}/topups", post(handle_post_topup))
        .route("/sessions/bootstrap", post(handle_post_bootstrap))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1_compat::session_store::InMemorySessionStore;

    fn test_state() -> Arc<SessionApiState> {
        Arc::new(SessionApiState {
            sessions: Arc::new(InMemorySessionStore::new()),
            metric: "milliseconds".to_owned(),
        })
    }

    #[tokio::test]
    async fn get_session_not_found() {
        let state = test_state();
        let result = state.sessions.get("aa:bb:cc:dd:ee:ff").await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn usage_report_roundtrip() {
        let state = test_state();
        state
            .sessions
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 5000, now_secs())
            .await
            .unwrap();

        let snapshot = ExternalUsageSnapshot {
            input_octets: 100,
            output_octets: 200,
            session_time: 5,
            reported_at: now_secs(),
        };
        let updated = state
            .sessions
            .record_external_usage("aa:bb:cc:dd:ee:ff", snapshot.clone())
            .await
            .unwrap()
            .expect("session exists");

        assert_eq!(updated.last_external_usage, Some(snapshot));
        assert_eq!(
            updated.allotment, 5000,
            "usage report must not clobber allotment"
        );
    }

    #[test]
    fn mac_normalization() {
        assert_eq!(normalize_mac("AA-BB-CC-DD-EE-FF"), "aa:bb:cc:dd:ee:ff");
        assert_eq!(normalize_mac("AABB.CCDD.EEFF"), "aabb:ccdd:eeff");
    }
}
