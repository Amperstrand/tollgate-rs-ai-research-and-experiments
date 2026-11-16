//! REST API for external session management (tollgate-auth consumer).
//!
//! Provides api-key-gated endpoints for querying sessions, reporting
//! external usage, and managing allotments. This is the REST surface
//! that tollgate-auth's Delegated Mode uses.
//!
//! Authentication: `X-API-Key` header checked against the
//! `TOLLGATE_API_KEY` environment variable (read once at router
//! construction; fail-closed when unset).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use super::adapter;
use super::handlers::extract_token_amount_and_allotment;
use super::merchant::{CustomerSession, ExternalUsageSnapshot, V1ServerConfig};
use super::reconcile;
use super::session_store::{SessionStore, now_secs};
use crate::driver::Driver;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SessionApiState {
    pub sessions: Arc<dyn SessionStore>,
    pub config: Arc<V1ServerConfig>,
    pub driver: Driver,
    pub api_key: Option<String>,
    pub journal_path: std::path::PathBuf,
}

impl SessionApiState {
    fn is_authorized(&self, headers: &HeaderMap) -> bool {
        let Some(key) = &self.api_key else {
            return false;
        };
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|k| k == key)
            .unwrap_or(false)
    }

    /// Redeem a Cashu token through the mainline driver (the money
    /// authority) with the pending-receive journal bracketing the
    /// receive: if the process dies mid-redeem, the journal entry
    /// survives for startup reconciliation (NUT-07 ambiguous-outcome
    /// pattern, same as the portal payment handler).
    async fn redeem(&self, token: &str, mac: &str) -> Result<(), String> {
        // Driver peers are keyed by mac_to_peer_hex (the portal
        // convention: a 66-char pubkey hex). A raw colon-MAC fails
        // parse_peer's hex::decode, and the core's ack then enqueues
        // under peer_to_hex(zeroed PeerId) — a different outbox key
        // than the caller drains — so even a verified token answers
        // "no response from gateway".
        let peer_hex = adapter::mac_to_peer_hex(mac);
        let entry = reconcile::PendingReceive {
            token: token.to_owned(),
            peer_hex: peer_hex.clone(),
            timestamp: now_secs(),
            mac: Some(mac.to_owned()),
        };
        let _ = reconcile::journal_pending_receive(&self.journal_path, &entry);

        let result = adapter::admit_peer_with_token(&self.driver, &peer_hex, None, token).await;

        let _ = reconcile::complete_pending_receive(&self.journal_path, token);

        if result.accepted {
            Ok(())
        } else {
            Err(result.reason.unwrap_or_else(|| "rejected".to_owned()))
        }
    }

    /// Credit an owed receive recovered from the startup journal (crash
    /// between receive and grant). The driver credit always happens;
    /// delegated entries (mac set) additionally credit the session
    /// store that tollgate-auth reads.
    pub async fn grant_owed_receive(
        &self,
        entry: &reconcile::PendingReceive,
        amount_sat: u64,
    ) -> Result<(), String> {
        if amount_sat == 0 {
            return Err("owed amount is zero — refusing to grant".to_owned());
        }
        self.driver
            .grant_owed_session(&entry.peer_hex, amount_sat * 1000)
            .await;
        if let Some(mac) = &entry.mac {
            let mint_url = self
                .config
                .accepted_mints
                .first()
                .map(|m| m.url.as_str())
                .unwrap_or("");
            let allotment =
                super::merchant::calculate_allotment(amount_sat, mint_url, &self.config)
                    .map_err(|e| e.to_string())?;
            self.sessions
                .add_allotment(mac, &self.config.metric, allotment, now_secs())
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
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
    pub access_level: String,
    pub allotment: u64,
    pub remaining_quota: i64,
    pub metric: String,
    pub next_checkin_ms: u64,
    pub is_final: bool,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_usage_update: Option<String>,
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
    let now = now_secs();
    let (remaining, access_level) = if session.metric == "milliseconds" {
        // saturating: a start_time ahead of `now` (clock step) must not
        // panic the unauthenticated GET path with a subtract overflow.
        let elapsed_ms = now.saturating_sub(session.start_time).saturating_mul(1000);
        let rem = (session.allotment as i64).saturating_sub(elapsed_ms);
        let level = if rem <= 0 { "suspended" } else { "active" };
        (rem, level.to_owned())
    } else {
        // saturating: reported octets are external input — u64::MAX values
        // must not overflow the sum on every subsequent session read.
        let usage = session
            .last_external_usage
            .as_ref()
            .map(|u| u.input_octets.saturating_add(u.output_octets))
            .unwrap_or(0);
        let rem =
            (session.allotment as i64).saturating_sub(i64::try_from(usage).unwrap_or(i64::MAX));
        let level = if rem <= 0 { "suspended" } else { "active" };
        (rem, level.to_owned())
    };
    // m2 contract: fixed check-in interval the RADIUS side schedules against
    let next_checkin_ms = 5_000u64;
    let is_final = remaining <= 0;

    SessionResponse {
        session_id: session.mac_address.clone(),
        access_level,
        allotment: session.allotment,
        remaining_quota: remaining,
        metric: session.metric.clone(),
        next_checkin_ms,
        is_final,
        created_at: session.start_time,
        last_usage_update: session
            .last_external_usage
            .as_ref()
            .map(|u| u.reported_at.to_string()),
    }
}

fn store_error(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{{\"error\":\"session store error: {e}\"}}"),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET is unauthenticated by contract (m2 + tollgate-auth): the client
/// identifies the session via the path MAC and sends no API key on reads.
async fn handle_get_session(
    State(state): State<Arc<SessionApiState>>,
    Path(mac): Path<String>,
) -> Response {
    let mac = normalize_mac(&mac);

    match state.sessions.get(&mac).await {
        Ok(Some(session)) => json_ok(&build_session_response(&session)),
        Ok(None) => error_json(StatusCode::NOT_FOUND, "session not found", Some(&mac)),
        Err(e) => store_error(e),
    }
}

async fn handle_post_usage(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
    Json(report): Json<UsageReport>,
) -> Response {
    if !state.is_authorized(&headers) {
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
        Ok(Some(updated)) => json_ok(&build_session_response(&updated)),
        Ok(None) => error_json(StatusCode::NOT_FOUND, "session not found", Some(&mac)),
        Err(e) => store_error(e),
    }
}

async fn handle_post_topup(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
    Json(req): Json<TopupRequest>,
) -> Response {
    if !state.is_authorized(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&mac);

    if let Err(reason) = state.redeem(&req.token, &mac).await {
        return error_json(
            StatusCode::PAYMENT_REQUIRED,
            &format!("payment rejected: {reason}"),
            Some(&mac),
        );
    }

    let (_, allotment) = extract_token_amount_and_allotment(&req.token, &state.config);
    match state
        .sessions
        .add_allotment(&mac, &state.config.metric, allotment, now_secs())
        .await
    {
        Ok(session) => json_ok(&build_session_response(&session)),
        Err(e) => store_error(e),
    }
}

async fn handle_delete_session(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Path(mac): Path<String>,
) -> Response {
    if !state.is_authorized(&headers) {
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
        Err(e) => store_error(e),
    }
}

async fn handle_post_bootstrap(
    State(state): State<Arc<SessionApiState>>,
    headers: HeaderMap,
    Json(req): Json<BootstrapRequest>,
) -> Response {
    if !state.is_authorized(&headers) {
        return error_json(StatusCode::UNAUTHORIZED, "unauthorized", None);
    }
    let mac = normalize_mac(&req.mac);

    // The server meters and prices in ONE configured metric; a caller-
    // selected metric would buy allotment at the configured step size but
    // be metered in the requested units. Reject the mismatch before
    // redeeming.
    let metric = match req.metric {
        Some(ref m) if *m != state.config.metric => {
            return error_json(
                StatusCode::BAD_REQUEST,
                &format!(
                    "unsupported metric '{m}': this TollGate meters in '{}'",
                    state.config.metric
                ),
                Some(&mac),
            );
        }
        _ => state.config.metric.clone(),
    };

    if let Err(reason) = state.redeem(&req.token, &mac).await {
        return error_json(
            StatusCode::PAYMENT_REQUIRED,
            &format!("payment rejected: {reason}"),
            Some(&mac),
        );
    }

    let (_, allotment) = extract_token_amount_and_allotment(&req.token, &state.config);
    match state
        .sessions
        .add_allotment(&mac, &metric, allotment, now_secs())
        .await
    {
        Ok(session) => json_ok(&build_session_response(&session)),
        Err(e) => store_error(e),
    }
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
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> Arc<SessionApiState> {
        let identity = Arc::new(
            crate::config::Identity::load_or_generate(&crate::config::Config::default()).unwrap(),
        );
        let driver = crate::driver::Driver::new(
            crate::wallet::BootstrapWallet::new(vec![]),
            crate::adapter::IpAdapter::new(),
            identity,
            tollgate_core::Price::default(),
            "bytes",
            Vec::new(),
        );
        Arc::new(SessionApiState {
            sessions: Arc::new(InMemorySessionStore::new()),
            config: crate::server::build_v1_config(&crate::config::V1CompatConfig::default(), None),
            driver,
            api_key: Some("test-key".to_owned()),
            journal_path: std::env::temp_dir()
                .join(format!("tg-sessapi-test-{}.jsonl", std::process::id())),
        })
    }

    fn bootstrap_body() -> &'static str {
        r#"{"access_method":"wifi","token":"cashuAgarbage","mac":"AA-BB-CC-DD-EE-FF"}"#
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

    #[tokio::test]
    async fn bootstrap_rejects_missing_key() {
        let app = build_session_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(bootstrap_body()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bootstrap_rejects_metric_mismatch() {
        let app = build_session_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/bootstrap")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(
                        r#"{"access_method":"wifi","token":"cashuAgarbage","mac":"AA-BB-CC-DD-EE-FF","metric":"bytes"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // config defaults to milliseconds; requesting bytes pricing must
        // fail BEFORE any redemption attempt.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn bootstrap_rejects_invalid_token() {
        let state = test_state();
        let app = build_session_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/bootstrap")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(bootstrap_body()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Empty wallet rejects every token; no session may be created.
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        assert!(
            state
                .sessions
                .get("aa:bb:cc:dd:ee:ff")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn grant_owed_credits_driver_and_store_for_delegated() {
        let state = test_state();
        let entry = reconcile::PendingReceive {
            token: "cashuBowed".to_owned(),
            peer_hex: adapter::mac_to_peer_hex("aa:bb:cc:dd:ee:ff"),
            timestamp: now_secs(),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
        };
        state.grant_owed_receive(&entry, 21).await.unwrap();

        let session = state
            .sessions
            .get("aa:bb:cc:dd:ee:ff")
            .await
            .unwrap()
            .expect("store credited");
        assert_eq!(session.allotment, 21 * 60_000);

        let status = state.driver.status().await;
        let peer = status
            .peers
            .into_iter()
            .find(|p| p.pubkey == entry.peer_hex)
            .expect("driver peer exists");
        assert_eq!(peer.their_balance, 21_000);
    }

    #[tokio::test]
    async fn grant_owed_portal_entry_credits_driver_only() {
        let state = test_state();
        let peer_hex = adapter::mac_to_peer_hex("aa:bb:cc:dd:ee:ff");
        let entry = reconcile::PendingReceive {
            token: "cashuBowed".to_owned(),
            peer_hex: peer_hex.clone(),
            timestamp: now_secs(),
            mac: None,
        };
        state.grant_owed_receive(&entry, 5).await.unwrap();
        assert!(
            state
                .sessions
                .get("aa:bb:cc:dd:ee:ff")
                .await
                .unwrap()
                .is_none(),
            "portal entries must not create store sessions"
        );
        let status = state.driver.status().await;
        let peer = status
            .peers
            .into_iter()
            .find(|p| p.pubkey == peer_hex)
            .expect("driver peer exists");
        assert_eq!(peer.their_balance, 5_000);
    }

    #[tokio::test]
    async fn grant_owed_zero_amount_is_refused() {
        let state = test_state();
        let entry = reconcile::PendingReceive {
            token: "cashuBowed".to_owned(),
            peer_hex: adapter::mac_to_peer_hex("aa:bb:cc:dd:ee:ff"),
            timestamp: now_secs(),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
        };
        assert!(state.grant_owed_receive(&entry, 0).await.is_err());
        assert!(
            state
                .sessions
                .get("aa:bb:cc:dd:ee:ff")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn router_serves_the_shared_store() {
        let state = test_state();
        let app = build_session_router(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sessions/aa:bb:cc:dd:ee:ff")
                    .header("x-api-key", "test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        state
            .sessions
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, now_secs())
            .await
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/aa:bb:cc:dd:ee:ff")
                    .header("x-api-key", "test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_session_needs_no_api_key() {
        let state = test_state();
        state
            .sessions
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, now_secs())
            .await
            .unwrap();
        let app = build_session_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/aa:bb:cc:dd:ee:ff")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn topup_rejects_invalid_token() {
        let app = build_session_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/aa:bb:cc:dd:ee:ff/topups")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(r#"{"token":"cashuAgarbage"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    }
    #[test]
    fn build_session_response_survives_hostile_fields() {
        // Regression (panic audit 2026-11-16, fuzz-confirmed 9334/25000):
        // u64::MAX octets overflowed the input+output sum and a future
        // start_time overflowed the elapsed subtraction — every later GET
        // /v1/sessions/{mac} panicked.
        let bytes_session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: 0,
            metric: "bytes".to_owned(),
            allotment: 5_000,
            last_external_usage: Some(ExternalUsageSnapshot {
                input_octets: u64::MAX,
                output_octets: 1,
                session_time: 0,
                reported_at: 0,
            }),
        };
        let resp = build_session_response(&bytes_session);
        assert_eq!(resp.access_level, "suspended", "max usage suspends");
        assert!(resp.remaining_quota <= 0);

        let future_session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: now_secs() + 86_400,
            metric: "milliseconds".to_owned(),
            allotment: 60_000,
            last_external_usage: None,
        };
        let resp = build_session_response(&future_session);
        assert_eq!(resp.access_level, "active", "future start is not expired");
        assert!(resp.remaining_quota > 0);
    }
}
