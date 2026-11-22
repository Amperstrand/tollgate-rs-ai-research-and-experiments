//! v1_compat HTTP handlers — the Go-v1 wire-protocol surface.
//!
//! DUAL RESPONSE FORMATS BY DESIGN: the Go v1 client contract requires
//! plain-text responses on some endpoints (`GET /usage` → `"u/a"`,
//! `"-1/-1"` when absent) while the rest of this server answers JSON.
//! Do NOT "unify" these — the text arms are load-bearing protocol
//! compatibility (verified end-to-end against the v1 client, 2026-11-15;
//! breaking them breaks every Go-lineage client at its first poll).

use super::ln_grant;
use super::reconcile;
use super::session_store::{SessionStore, now_secs};
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

pub(crate) fn extract_token_amount_and_allotment(
    token: &str,
    config: &V1ServerConfig,
) -> (u64, u64) {
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
    // cashu::Token parses both V3 (cashuA JSON) and V4 (cashuB binary);
    // cdk 0.18 wallets emit V4, which the legacy path below cannot read.
    if let Ok(parsed) = token.parse::<cashu::nuts::Token>() {
        if let Ok(amount) = parsed.value() {
            return Some(amount.into());
        }
    }
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

pub fn build_router(
    driver: Driver,
    config: Arc<V1ServerConfig>,
    sessions: Option<Arc<dyn SessionStore>>,
) -> Router {
    Router::new()
        .route(
            "/",
            get(handle_get_details)
                .post(handle_post_payment)
                // HTTP-01 corpus (issue #154): POST / reads up to 1 MiB and
                // answers oversize with 400 + invalid-request notice — it
                // never 413s (deliberate asymmetry with /ln-invoice's 8 KiB
                // 413). The handler enforces the exact cap; this layer only
                // keeps axum from rejecting before the handler runs.
                .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route("/pay", get(handle_get_details))
        .route("/usage", get(handle_usage))
        .route("/whoami", get(handle_whoami))
        .route("/balance", get(handle_balance))
        .route(
            "/ln-invoice",
            get(handle_get_ln_invoice).post(handle_post_ln_invoice),
        )
        .route("/log-beacon", axum::routing::post(handle_log_beacon))
        .layer(axum::Extension(config))
        .layer(axum::Extension(sessions))
        .layer(axum::Extension(Arc::new(ln_grant::LnGrantRegistry::new())))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(driver)
}

/// X-TollGate-MAC header (delegated callers — tollgate-auth Go
/// binaries): trimmed, non-empty, normalized to colon-form lowercase
/// (m2 contract). Overrides IP-based MAC resolution when present.
fn header_mac(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-tollgate-mac")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase().replace(['-', '.'], ":"))
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

/// The pending-receive journal entry for a portal payment. Delegated
/// (header-identified) payments carry the colon-form MAC so startup
/// reconciliation (`grant_owed_receive`) credits the session-API store
/// the delegated consumer reads, not just the driver; IP-resolved
/// (portal) payments leave it unset.
fn payment_journal_entry(
    token: &str,
    peer_hex: &str,
    hdr_mac: Option<&str>,
) -> reconcile::PendingReceive {
    reconcile::PendingReceive {
        token: token.to_owned(),
        peer_hex: peer_hex.to_owned(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        mac: hdr_mac.map(str::to_owned),
    }
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
    axum::Extension(sessions): axum::Extension<Option<Arc<dyn SessionStore>>>,
    connect_info: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let connect_info = connect_info.map(|axum::Extension(info)| info);
    // HTTP-01 payment contract (corpus http01-payment.json): allowed
    // Content-Types are text/plain, application/json, and absent —
    // anything else is rejected 415 BEFORE token processing, with the
    // exact Go-v1 error body. Live-found via consumer-conformance
    // (issue #154): the fork answered 400 "missing or invalid token".
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        let base = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !base.is_empty() && base != "text/plain" && base != "application/json" {
            let json = serde_json::json!({"error": "Unsupported Media Type"});
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    }
    // HTTP-01 corpus (issue #154): POST / caps the body at 1 MiB and
    // answers oversize 400 + invalid-request notice — never 413 (deliberate
    // asymmetry with /ln-invoice). The route layer admits 2 MiB so this
    // check, not axum, produces the contract response.
    if body.len() > 1_048_576 {
        return match merchant::build_notice_event(
            "error",
            "invalid-request",
            "request body exceeds 1 MiB",
            None,
            &config,
        ) {
            Ok(json) => (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                json,
            )
                .into_response(),
            Err(_) => json_error(StatusCode::BAD_REQUEST, "invalid request"),
        };
    }
    let token = match extract_payment_token(&body) {
        Some(t) => t,
        None => {
            // HTTP-01 corpus: token-decode failures answer as a SIGNED
            // kind-21023 Notice event with level=error and
            // code=payment-error-invalid-token (Go v1 merchant.go 1509),
            // not a plain JSON error envelope.
            return match merchant::build_notice_event(
                "error",
                "payment-error-invalid-token",
                "missing or invalid token",
                None,
                &config,
            ) {
                Ok(json) => (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    json,
                )
                    .into_response(),
                Err(_) => json_error(StatusCode::BAD_REQUEST, "missing or invalid token"),
            };
        }
    };

    // Portal payments resolve the payer like /usage does: trusted
    // XFF/X-Real-IP, then the TCP connection address. Headers-only
    // resolution rejected every direct v1-client payment under the
    // default trust_proxy_headers=false (live-found 2026-11-17).
    let client_ip = extract_ip_full(&headers, connect_info.as_ref(), config.trust_proxy_headers);

    // X-TollGate-MAC (delegated callers) states which device the
    // payment is for, overriding IP-based resolution. The caller's
    // own IP must not be mapped as the device's, so the admission
    // path gets no IP on this route.
    let hdr_mac = header_mac(&headers);
    let (peer_hex, peer_ip) = match hdr_mac.clone() {
        Some(mac) => (adapter::mac_to_peer_hex(&mac), None),
        None => match adapter::resolve_peer_hex(client_ip).await {
            Some(hex) => (hex, client_ip),
            None => {
                let json = serde_json::json!({"error": "could not resolve client identity"});
                return (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    serde_json::to_string(&json).unwrap_or_default(),
                )
                    .into_response();
            }
        },
    };

    // Journal the intent BEFORE the receive — if the process dies between
    // the receive and the response, startup reconciliation checks NUT-07
    // and grants the owed session (Go v1 ambiguous-outcome pattern).
    // Delegated (header-identified) payments carry the MAC so recovery
    // also credits the session store; IP-resolved payments leave it unset.
    // Fail-closed (issue #141): a receive that cannot be journaled is
    // refused — proceeding unjournaled would reopen the crash-loss
    // window the journal exists to close.
    let journal_path = config
        .journal_path
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new(reconcile::DEFAULT_PENDING_RECEIVES_FILE));
    if let Err(e) = reconcile::journal_pending_receive(
        journal_path,
        &payment_journal_entry(&token, &peer_hex, hdr_mac.as_deref()),
    ) {
        tracing::error!(
            journal = %journal_path.display(),
            error = %e,
            "pending-receive journal write failed — failing payment (fail-closed)"
        );
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "payment journal unavailable",
        );
    }

    let result = adapter::admit_peer_with_token(&driver, &peer_hex, peer_ip, &token).await;

    if result.accepted {
        let (amount_sat, allotment) = extract_token_amount_and_allotment(&token, &config);
        // Delegated (header-identified) payments also credit the
        // session-API store — the surface tollgate-auth reads via
        // GetSession/ReportUsage. m2 had one session system; this
        // keeps both ledgers consistent for delegated callers.
        // The credit lands BEFORE the journal entry is completed (the
        // #129 ordering): a crash between the receive and the credit is
        // re-derived at startup from the journaled mac, and the
        // token-keyed credit absorbs the re-derivation. Completing
        // first would make that window unrecoverable; a raw
        // add_allotment would double-credit on re-derivation.
        if let (Some(mac), Some(store)) = (&hdr_mac, &sessions) {
            match store
                .credit_for_token(mac, &config.metric, allotment, &token, now_secs())
                .await
            {
                Ok(_) => {
                    let _ = reconcile::complete_pending_receive(journal_path, &token);
                }
                Err(e) => {
                    // Credit failed: keep the entry journaled so startup
                    // recovery re-derives the owed store credit.
                    tracing::warn!(
                        mac,
                        error = %e,
                        "delegated store credit failed — journal entry kept for recovery"
                    );
                }
            }
        } else {
            // Journal cleanup: remove the entry on a definitive outcome
            // (accepted or rejected — either way the token's fate is known).
            let _ = reconcile::complete_pending_receive(journal_path, &token);
        }

        let session = merchant::CustomerSession {
            mac_address: peer_hex.clone(),
            start_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            metric: config.metric.clone(),
            allotment,
            last_external_usage: None,
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
        // Journal cleanup: a rejection is a definitive outcome (nothing
        // was consumed by this payment), so nothing can be owed.
        let _ = reconcile::complete_pending_receive(journal_path, &token);
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
    axum::Extension(sessions): axum::Extension<Option<Arc<dyn SessionStore>>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_ip = extract_ip_full(
        &headers,
        Some(&ConnectInfo(addr)),
        config.trust_proxy_headers,
    );
    // Go v1 parity: unresolved MAC and missing session both answer
    // 200 "-1/-1" — clients parse "no session" from it, not an error.
    //
    // Delegated (header-identified) callers with a store session read
    // METRIC units derived from the session store — the persistent
    // ledger — in the Go v1 GetUsage shape "consumed/allotment"
    // (tollgate-module-basic-go merchant.go). The driver's msat
    // balance must never leak into these numbers: live 2026-11-17
    // (RADIUS delegated E2E) an 8-sat session (480000 ms allotment)
    // answered "0/7000" and tollgate-auth's reconnect parsed 7 s of
    // remaining time. A store session wins even when the driver peer
    // is gone (e.g. after a restart) — the store is the ledger.
    if let Some(mac) = header_mac(&headers) {
        if let Some(store) = &sessions {
            match store.get(&mac).await {
                Ok(Some(session)) => {
                    let (consumed, _) = super::session_api::session_metric_usage(&session);
                    return (
                        StatusCode::OK,
                        [("content-type", "text/plain")],
                        format!("{consumed}/{}", session.allotment),
                    )
                        .into_response();
                }
                // No store session (LN grants never credit it, portal
                // payments leave mac unset) — fall through to the
                // driver view below.
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        mac,
                        error = %e,
                        "session store read failed; falling back to driver view"
                    );
                }
            }
        }
        return driver_usage_response(&driver, &adapter::mac_to_peer_hex(&mac)).await;
    }
    match adapter::resolve_peer_hex(client_ip).await {
        Some(peer_hex) => driver_usage_response(&driver, &peer_hex).await,
        None => usage_no_session(),
    }
}

/// The pre-store /usage view: the DRIVER peer's balance (msat) as
/// "consumed/balance". Kept unchanged for IP-resolved (portal) callers
/// and for header callers with no store session.
async fn driver_usage_response(driver: &Driver, peer_hex: &str) -> Response {
    match adapter::get_usage_json(driver, peer_hex).await {
        Some(usage) => {
            // Go v1 contract: plain text "usage/allotment" — the v1 client
            // (and Go clients) parse exactly this shape (VM E2E 2026-11-15:
            // the JSON body broke the client's poll loop). The driver
            // reports allotment == balance == remaining while metering, so
            // usage = consumed = allotment - balance.
            let consumed = usage.allotment.saturating_sub(usage.balance);
            (
                StatusCode::OK,
                [("content-type", "text/plain")],
                format!("{consumed}/{}", usage.balance),
            )
                .into_response()
        }
        None => usage_no_session(),
    }
}

fn usage_no_session() -> Response {
    // Go v1 contract: /usage without a session answers 200 "-1/-1"
    // plain text — the v1 client (and Go clients) parse this as
    // "no session", not as an error. Found via VM E2E 2026-11-14:
    // the 404 JSON broke the client at its very first step.
    (
        StatusCode::OK,
        [("content-type", "text/plain")],
        "-1/-1".to_owned(),
    )
        .into_response()
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
    // Delegated (header-identified) callers resolve exactly like the
    // payment path: the X-TollGate-MAC names the device, so the caller's
    // own IP is never mapped as the device's. Live-found on the PRTA
    // physical rig (issue #150): a session created via POST / with the
    // MAC header was invisible to /balance's IP-only lookup, which
    // answered {"error":"unknown client"} with the same headers.
    let peer_hex = match header_mac(&headers) {
        Some(mac) => adapter::mac_to_peer_hex(&mac),
        None => match adapter::resolve_peer_hex(client_ip).await {
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
        },
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
            // Go v1 contract: the client's LnInvoiceResponse requires
            // state + access_granted on this arm too (found via VM E2E
            // 2026-11-14 — the parse failed on the missing field).
            let json = serde_json::json!({
                "status": 1,
                "quote": info.quote_id,
                "invoice": info.invoice,
                "mint_url": mint_url,
                "amount": info.amount,
                "expiry": info.expiry,
                "state": "UNPAID",
                "access_granted": false,
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
    State(driver): State<Driver>,
    axum::Extension(config): axum::Extension<Arc<V1ServerConfig>>,
    axum::Extension(grant_registry): axum::Extension<Arc<ln_grant::LnGrantRegistry>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let quote_id = match params.get("quote") {
        Some(id) => id.as_str(),
        None => {
            // HTTP-03 corpus (issue #154): the error body carries a numeric
            // `status` field (0 = quote-level failure) alongside the exact
            // Go-v1 error text.
            let json = serde_json::json!({"status": 0, "error": "quote is required"});
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    // HTTP-03 corpus (issue #161): the quote-status lookup answers its
    // contract shape even in degraded mode — service-unavailable precedes
    // token PROCESSING (HTTP-01), not the quote-status lookup. With no
    // wallet, unknown quotes still answer 404 {"status":0,"error":
    // "failed to fetch invoice status"}; missing-param keeps its 400.
    let wallet = match config.wallet.as_ref() {
        Some(w) => w.clone(),
        None => {
            let json = serde_json::json!({
                "status": 0,
                "error": "failed to fetch invoice status"
            });
            return (
                StatusCode::NOT_FOUND,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    let state = match wallet.check_mint_quote_status(quote_id).await {
        Ok(state) => state,
        Err(e) => {
            // HTTP-03 corpus (issue #154): unknown quotes answer 404 with
            // the Go-v1 body {"status":0,"error":"failed to fetch invoice
            // status"} — other failures keep their text but gain status:0.
            let unknown = format!("{e}").to_lowercase().contains("unknown");
            let error_text = if unknown {
                "failed to fetch invoice status".to_owned()
            } else {
                format!("quote check failed: {e}")
            };
            let json = serde_json::json!({"status": 0, "error": error_text});
            let status = if unknown {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return (
                status,
                [("content-type", "application/json")],
                serde_json::to_string(&json).unwrap_or_default(),
            )
                .into_response();
        }
    };

    if state != crate::v1_compat::wallet::QuoteState::Unpaid {
        // A paid quote grants access exactly once: mint the proofs,
        // re-serialize them as a bootstrap token, and admit the peer
        // through the Driver — the same verification path a Cashu POST
        // payment takes. Concurrent pollers are serialized by the registry.
        let client_ip = extract_ip_full(
            &headers,
            Some(&ConnectInfo(addr)),
            config.trust_proxy_headers,
        );
        let peer_hex = match adapter::resolve_peer_hex(client_ip).await {
            Some(hex) => hex,
            None => {
                return json_error(StatusCode::BAD_REQUEST, "could not resolve client identity");
            }
        };
        let outcome = ln_grant::grant_if_paid(
            &grant_registry,
            &ln_grant::GrantContext {
                minter: &*wallet,
                admitter: &driver,
                peer_hex: &peer_hex,
                ip: client_ip,
                mint_url: wallet.mint_url(),
                config: &config,
            },
            quote_id,
            state,
        )
        .await;

        return match outcome {
            ln_grant::GrantOutcome::Issued { allotment }
            | ln_grant::GrantOutcome::AlreadyIssued { allotment } => {
                let json = serde_json::json!({
                    "quote": quote_id,
                    "state": "ISSUED",
                    "access_granted": true,
                    "allotment": allotment,
                    "metric": config.metric,
                });
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    serde_json::to_string(&json).unwrap_or_default(),
                )
                    .into_response()
            }
            ln_grant::GrantOutcome::Processing => {
                let json = serde_json::json!({
                    "quote": quote_id,
                    "state": "PAID",
                    "access_granted": false,
                });
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    serde_json::to_string(&json).unwrap_or_default(),
                )
                    .into_response()
            }
            ln_grant::GrantOutcome::Failed(reason) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("grant failed: {reason}"),
            ),
        };
    }

    let json = serde_json::json!({
        "quote": quote_id,
        "state": "UNPAID",
        "access_granted": false,
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::to_string(&json).unwrap_or_default(),
    )
        .into_response()
}

async fn handle_log_beacon(body: String) -> Response {
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => {
            let level = v.get("level").and_then(|l| l.as_str()).unwrap_or("info");
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let client = v
                .get("client")
                .and_then(|c| c.as_str())
                .unwrap_or("unknown");
            match level {
                "error" => tracing::error!(client = %client, "log-beacon: {msg}"),
                "warn" => tracing::warn!(client = %client, "log-beacon: {msg}"),
                _ => tracing::info!(client = %client, "log-beacon: {msg}"),
            }
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                r#"{"logged":true}"#,
            )
                .into_response()
        }
        Err(_) => json_error(StatusCode::BAD_REQUEST, "invalid JSON body"),
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
        test_config_with_journal(Some(
            std::env::temp_dir().join(format!("tg-portal-journal-{}.jsonl", std::process::id())),
        ))
    }

    fn test_config_with_journal(journal_path: Option<std::path::PathBuf>) -> Arc<V1ServerConfig> {
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
            journal_path,
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
    fn extract_ip_full_falls_back_to_connection_address() {
        let headers = HeaderMap::new();
        let conn = ConnectInfo(SocketAddr::from(([10, 1, 2, 3], 9999)));
        let ip = extract_ip_full(&headers, Some(&conn), false);
        assert_eq!(
            ip.map(|i| i.to_string()),
            Some("10.1.2.3".to_string()),
            "untrusted requests must resolve from the TCP connection address"
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
        let app = build_router(test_driver(), test_config(), None);
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
    async fn post_payment_unsupported_media_type_is_415() {
        // HTTP-01 corpus vector http01-post/unsupported-media-type (issue
        // #154): content-type outside {text/plain, application/json,
        // absent} answers 415 with the exact Go-v1 body BEFORE any token
        // processing — not 400 "missing or invalid token".
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("anything"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Unsupported Media Type");
    }

    #[tokio::test]
    async fn post_payment_charset_suffix_still_accepted() {
        // A media type with parameters (charset=utf-8) is still text/plain —
        // only the base type gates (corpus: allowed = text/plain,
        // application/json, absent).
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .header("content-type", "text/plain; charset=utf-8")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .body(Body::from("cashuAgarbage"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    async fn post_payment_header_mac_skips_ip_resolution() {
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "text/plain")
                    .header("x-tollgate-mac", "AA-BB-CC-DD-EE-FF")
                    .body(Body::from("cashuAgarbage"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The header resolves the peer, so the request reaches token
        // verification (402) instead of failing identity resolution
        // (400 "could not resolve client identity").
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    async fn balance_header_mac_resolves_delegated_peer() {
        // Issue #150: a delegated caller identifying by X-TollGate-MAC
        // must resolve through the header — the payment path's peer —
        // not through the caller's (unmapped) IP. With no paid session
        // the honest answer is 404 "no active session", NOT 400
        // "unknown client".
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/balance")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .header("x-forwarded-for", "10.99.99.100")
                    .header("x-tollgate-mac", "de:54:4e:91:49:da")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "no active session");
    }

    #[tokio::test]
    async fn balance_unmapped_ip_without_header_is_unknown_client() {
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/balance")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .header("x-forwarded-for", "10.99.99.100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown client");
    }

    #[tokio::test]
    async fn post_payment_fails_closed_when_journal_unwritable() {
        // Issue #141: an unwritable pending-receive journal must fail
        // the payment (503) — proceeding unjournaled would lose the
        // credit silently on a crash between mint-spend and credit.
        let dir = std::env::temp_dir().join(format!("tg-portal-jfail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let config = test_config_with_journal(Some(blocker.join("pending.jsonl")));
        let driver = test_driver();
        let app = build_router(driver.clone(), config, None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "text/plain")
                    .header("x-tollgate-mac", "AA-BB-CC-DD-EE-FF")
                    .body(Body::from("cashuAgarbage"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"], "payment journal unavailable",
            "503 must name the journal failure: {json}"
        );
        assert!(
            driver.status().await.peers.is_empty(),
            "the driver peer must not be admitted when journaling fails"
        );
    }

    #[tokio::test]
    async fn usage_header_mac_finds_granted_session() {
        let driver = test_driver();
        let peer_hex = adapter::mac_to_peer_hex("aa:bb:cc:dd:ee:ff");
        driver.grant_owed_session(&peer_hex, 60_000).await;
        let app = build_router(driver, test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        assert_ne!(
            &body[..],
            b"-1/-1",
            "header-identified peer must resolve to the granted session"
        );
    }

    #[tokio::test]
    async fn usage_header_mac_with_store_session_reports_metric_ms() {
        // Live-proven 2026-11-17 (RADIUS delegated E2E): the driver's
        // msat balance leaked into /usage as if it were metric ms — an
        // 8-sat session (480000 ms allotment) answered "0/7000" and
        // tollgate-auth's reconnect parsed 7 s of remaining time.
        // Header-identified callers with a store session must read
        // metric units from the session, Go v1 GetUsage shape
        // "elapsed/allotment" (tollgate-module-basic-go merchant.go).
        let store = Arc::new(crate::v1_compat::session_store::InMemorySessionStore::new());
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, now_secs() - 10)
            .await
            .unwrap();
        let app = build_router(test_driver(), test_config(), Some(store));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        let (consumed, allotment) = body
            .split_once('/')
            .unwrap_or_else(|| panic!("usage must be 'consumed/allotment', got {body}"));
        let consumed: i64 = consumed.parse().unwrap();
        let allotment: i64 = allotment.parse().unwrap();
        assert_eq!(
            allotment, 60_000,
            "second number is the allotment, got {body}"
        );
        // 10 s have elapsed; elapsed is measured at whole-second
        // granularity, so a second boundary during the test may read
        // as 11 s.
        assert!(
            (9_900..=11_500).contains(&consumed),
            "consumed must track elapsed wall-clock ms, got {body}"
        );
        assert!(
            allotment - consumed >= 60_000 - 11_500,
            "remaining derived as allotment - consumed must stay ms-scale, got {body}"
        );
    }

    #[tokio::test]
    async fn usage_header_mac_with_bytes_session_reports_byte_units() {
        let store = Arc::new(crate::v1_compat::session_store::InMemorySessionStore::new());
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 5_000, now_secs())
            .await
            .unwrap();
        store
            .record_external_usage(
                "aa:bb:cc:dd:ee:ff",
                merchant::ExternalUsageSnapshot {
                    input_octets: 100,
                    output_octets: 200,
                    session_time: 0,
                    reported_at: now_secs(),
                },
            )
            .await
            .unwrap();
        let app = build_router(test_driver(), test_config(), Some(store));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(body, "300/5000", "bytes sessions report consumed/allotment");
    }

    #[tokio::test]
    async fn usage_header_mac_without_store_session_keeps_driver_view() {
        // Header callers whose session is NOT in the store (LN grants
        // never credit it) keep today's driver view.
        let driver = test_driver();
        let peer_hex = adapter::mac_to_peer_hex("aa:bb:cc:dd:ee:ff");
        driver.grant_owed_session(&peer_hex, 60_000).await;
        let app = build_router(
            driver,
            test_config(),
            Some(Arc::new(
                crate::v1_compat::session_store::InMemorySessionStore::new(),
            )),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        // Driver view: consumed = allotment - balance = 0, balance in msat.
        assert_eq!(body, "0/60000");
    }

    #[tokio::test]
    async fn usage_header_mac_no_session_no_peer_answers_minus_one() {
        let app = build_router(
            test_driver(),
            test_config(),
            Some(Arc::new(
                crate::v1_compat::session_store::InMemorySessionStore::new(),
            )),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/usage")
                    .header("x-tollgate-mac", "aa:bb:cc:dd:ee:ff")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        assert_eq!(&body[..], b"-1/-1");
    }

    #[tokio::test]
    async fn get_usage_returns_4xx_without_session() {
        let app = build_router(test_driver(), test_config(), None);
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
        // Go v1 parity: no session answers 200 "-1/-1" plain text
        // (contract change 2026-11-15, VM E2E — clients parse this as
        // "no session", not an error).
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        assert_eq!(&body[..], b"-1/-1");
    }

    #[tokio::test]
    async fn get_balance_returns_4xx_without_session() {
        let app = build_router(test_driver(), test_config(), None);
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
        assert!(
            resp.status().is_client_error(),
            "expected 4xx, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn post_ln_invoice_attempts_wallet_init() {
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ln-invoice?quote=abc123")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
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
        let app = build_router(test_driver(), test_config(), None);
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
        // HTTP-01 corpus (issue #154): the error body is a SIGNED kind-21023
        // Notice with level=error and code=payment-error-invalid-token.
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 21_023);
        assert!(json["id"].is_string(), "notice must be signed (id)");
        assert!(json["sig"].is_string(), "notice must be signed (sig)");
        assert!(
            json["tags"]
                .as_array()
                .expect("tags")
                .iter()
                .any(|t| t == &serde_json::json!(["level", "error"]))
        );
        assert!(
            json["tags"]
                .as_array()
                .expect("tags")
                .iter()
                .any(|t| t == &serde_json::json!(["code", "payment-error-invalid-token"]))
        );
    }

    #[test]
    fn header_payment_journal_entry_carries_mac() {
        // Regression (money-invariant audit 2026-11-16): the portal
        // payment path journaled mac: None even for delegated
        // (X-TollGate-MAC) payments, so crash recovery granted driver
        // credit only — the session store never saw the owed session.
        let entry = payment_journal_entry("cashuBhdr", "aabb", Some("02:50:00:00:00:42"));
        assert_eq!(entry.mac.as_deref(), Some("02:50:00:00:00:42"));
        assert_eq!(entry.token, "cashuBhdr");
        assert_eq!(entry.peer_hex, "aabb");
    }

    #[test]
    fn portal_payment_journal_entry_has_no_mac() {
        let entry = payment_journal_entry("cashuBportal", "aabb", None);
        assert_eq!(entry.mac, None);
    }

    #[test]
    fn header_payment_journal_entry_keeps_mac_on_disk() {
        // Inspect the journal FILE the way startup reconcile parses it:
        // the serialized entry a header payment writes must keep the mac.
        let dir = std::env::temp_dir().join(format!("tg-hdr-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending.jsonl");
        let entry = payment_journal_entry("cashuBhdr2", "aabb", Some("02:50:00:00:00:43"));
        reconcile::journal_pending_receive(&path, &entry).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(text.trim()).expect("journal line is valid JSON");
        assert_eq!(parsed["mac"], "02:50:00:00:00:43");
        assert_eq!(parsed["token"], "cashuBhdr2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn advertisement_has_metric_and_step_size_tags() {
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
    async fn post_payment_oversize_body_is_400_invalid_request_notice() {
        // HTTP-01 corpus body-over-1mib: 400 + signed notice with
        // code=invalid-request — POST / never 413s (issue #154).
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from(vec![b'x'; 1_048_577]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 21_023);
        assert!(
            json["tags"]
                .as_array()
                .expect("tags")
                .iter()
                .any(|t| t == &serde_json::json!(["code", "invalid-request"]))
        );
    }

    #[tokio::test]
    async fn post_payment_legacy_envelope_other_kind_is_invalid_token_notice() {
        // HTTP-01 corpus legacy-envelope-other-kind: a JSON body whose kind
        // is not 21000 is treated as a raw token string and fails decode ->
        // the signed payment-error-invalid-token notice.
        let app = build_router(test_driver(), test_config(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"kind":1,"tags":[["payment","x"]],"content":""}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], 21_023);
        assert!(
            json["tags"]
                .as_array()
                .expect("tags")
                .iter()
                .any(|t| t == &serde_json::json!(["code", "payment-error-invalid-token"]))
        );
    }

    #[tokio::test]
    async fn post_payment_rejects_empty_body() {
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
        let app = build_router(test_driver(), test_config(), None);
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
        // Go v1 parity: unresolved MAC answers 200 "-1/-1" as well.
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        assert_eq!(&body[..], b"-1/-1");
    }

    #[tokio::test]
    async fn balance_returns_400_for_unknown_peer() {
        let app = build_router(test_driver(), test_config(), None);
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
    async fn post_with_sub_1mib_body_is_not_413() {
        // HTTP-01 corpus (issue #154): POST / has no 413 — bodies under
        // 1 MiB flow to token processing regardless of the old 64 KiB
        // router cap (the 413 at 8 KiB belongs to /ln-invoice only). A
        // 70 KB non-token body reaches the decoder and answers the signed
        // invalid-token notice, not PAYLOAD_TOO_LARGE.
        let app = build_router(test_driver(), test_config(), None);
        let big_body = vec![b'A'; 70_000];
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
        assert_ne!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
        let tracker = std::sync::Arc::new(MintHealthTracker::new(std::slice::from_ref(&mint_url)));
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
            journal_path: None,
        });

        let app = build_router(test_driver(), config, None);
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
        assert_eq!(
            json["kind"], 10021,
            "degraded advertisement keeps kind 10021"
        );
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
            t.as_array().unwrap().first().and_then(|s| s.as_str()) == Some("price_per_step")
        });
        assert!(
            has_warning,
            "degraded advertisement must carry level=warning"
        );
        assert!(
            has_code,
            "degraded advertisement must carry code=no-reachable-mints"
        );
        assert!(
            !has_price,
            "degraded advertisement must NOT advertise price_per_step"
        );
    }

    #[test]
    fn regression_pr117_v4_token_amount_decodes() {
        // Regression (PR #117, live E2E 2026-11-16): decode_cashu_amount
        // only read V3 (cashuA JSON); cdk 0.18 wallets emit V4 (cashuB
        // binary), so every real token decoded to amount 0 → allotment 0
        // on the portal payment and delegated redemption paths. The
        // cashu::Token parse must read the V4 token's TRUE amount (the
        // legacy cashuA JSON fallback stays for old clients).
        let config = test_config();
        // Real 1-sat testnut V4 token (same fixture as wallet.rs's
        // SAMPLE_TOKEN).
        let v4 = "cashuBo2FteBtodHRwczovL3Rlc3RudXQuY2FzaHUuc3BhY2VhdWNzYXRhdIGiYWlIAYhKdLsvxe5hcIGkYWEBYXN4QDk1NTM1NzQ1YjQ2MzM2OGQ1OTVkMGVhMmQ1M2NmMDU0YjZkY2ZhZTY0NjhlOWU0N2U1MDc1YWU3OWRmNmUyODdhY1ghA03QgEalpQeCViTFYVixs-4tTxGmV0Dl-hKTQ8jLyG1ZYWSjYWVYIKlCWsnyOJRBHT_0xffz67uTQUWhk336QvZbnEQW6OUZYXNYIA88wEUIkwoL1RKs6j41AgtMZLp2e3JrlpZyU1o2M3TJYXJYILoalwd76VtIosztMCjHmQzbNUVKCM4VjvV02fSkG19-";
        assert_eq!(
            extract_token_amount_and_allotment(v4, &config),
            (1, 60_000),
            "a cashuB (V4) token must decode to its true amount, not 0"
        );

        // V3 (cashuA) tokens keep decoding through the legacy JSON shape.
        let v3 = crate::wallet::token_with_mint("https://mint.example");
        assert_eq!(extract_token_amount_and_allotment(&v3, &config).0, 1);
    }

    #[tokio::test]
    async fn regression_pr134_portal_payment_uses_connection_ip() {
        // Regression (PR #134, live 2026-11-17): handle_post_payment had
        // no ConnectInfo extractor, so with trust_proxy_headers=false
        // (the default) every direct v1-client payment 400'd "could not
        // resolve client identity" regardless of DHCP leases — only
        // delegated-header or trusted-proxy callers could pay. The payer
        // must be resolved like /usage: trusted headers, then the TCP
        // connection address. A caller whose connection IP has a DHCP
        // lease reaches token verification (402), not the 400.
        const LEASE_IP: &[u8; 4] = &[198, 51, 100, 50]; // TEST-NET-2: never a real lease
        let lease_path = std::path::Path::new("/tmp/dhcp.leases");
        let original_leases = std::fs::read_to_string(lease_path).ok();
        std::fs::write(
            lease_path,
            "1770000000 02:50:00:00:00:99 198.51.100.50 payer client-id\n",
        )
        .expect("test writes the dnsmasq lease fixture");

        // Hermetic journal: the handler journals the payment intent
        // before verification, so the test config points the journal at
        // a temp dir (the pre-journal_path fallback wrote the real
        // /var/lib/tollgate path — and referenced a const that no
        // longer exists, breaking the test build at 5efcb99).
        let journal_dir =
            std::env::temp_dir().join(format!("tg-pr134-journal-{}", std::process::id()));
        std::fs::create_dir_all(&journal_dir).unwrap();

        let app = build_router(
            test_driver(),
            test_config_with_journal(Some(journal_dir.join("pending.jsonl"))),
            None,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "text/plain")
                    .extension(ConnectInfo(SocketAddr::from((*LEASE_IP, 40000))))
                    .body(Body::from("cashuAgarbage"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Restore fixtures before asserting so a failed assert cannot
        // leak them.
        match original_leases {
            Some(contents) => std::fs::write(lease_path, contents).unwrap(),
            None => std::fs::remove_file(lease_path).unwrap(),
        }
        std::fs::remove_dir_all(&journal_dir).ok();

        assert_eq!(
            resp.status(),
            StatusCode::PAYMENT_REQUIRED,
            "connection IP + DHCP lease must resolve the payer to token verification (402), not 400 'could not resolve client identity'"
        );
        let body = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            !body.contains("could not resolve client identity"),
            "identity must be resolved from the connection address, got: {body}"
        );
    }

    /// Delegated-payment probe: an [`SessionStore`] wrapper that records
    /// every credit_for_token call and whether the pending-receive
    /// journal STILL contained the token at credit time — the observable
    /// for the #129/#131 ordering contract (credit lands before journal
    /// completion; completing first would make the credit's crash window
    /// unrecoverable).
    struct CreditOrderingProbe {
        inner: crate::v1_compat::session_store::InMemorySessionStore,
        journal_path: std::path::PathBuf,
        credits: std::sync::Mutex<Vec<(String, String, u64, String)>>,
        journal_held_token_at_credit: std::sync::Mutex<Vec<bool>>,
    }

    impl CreditOrderingProbe {
        fn new(journal_path: &std::path::Path) -> Self {
            Self {
                inner: crate::v1_compat::session_store::InMemorySessionStore::new(),
                journal_path: journal_path.to_owned(),
                credits: std::sync::Mutex::new(Vec::new()),
                journal_held_token_at_credit: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl SessionStore for CreditOrderingProbe {
        async fn credit_for_token(
            &self,
            mac: &str,
            metric: &str,
            amount: u64,
            token: &str,
            now: i64,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            let still_journaled = std::fs::read_to_string(&self.journal_path)
                .map(|c| c.lines().any(|l| l.contains(token)))
                .unwrap_or(false);
            self.credits.lock().unwrap().push((
                mac.to_owned(),
                metric.to_owned(),
                amount,
                token.to_owned(),
            ));
            self.journal_held_token_at_credit
                .lock()
                .unwrap()
                .push(still_journaled);
            self.inner
                .credit_for_token(mac, metric, amount, token, now)
                .await
        }

        async fn get(
            &self,
            mac: &str,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.get(mac).await
        }

        async fn insert(
            &self,
            session: merchant::CustomerSession,
        ) -> Result<(), crate::v1_compat::session_store::SessionStoreError> {
            self.inner.insert(session).await
        }

        async fn remove(
            &self,
            mac: &str,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.remove(mac).await
        }

        async fn update(
            &self,
            mac: &str,
            session: merchant::CustomerSession,
        ) -> Result<(), crate::v1_compat::session_store::SessionStoreError> {
            self.inner.update(mac, session).await
        }

        async fn list_all(
            &self,
        ) -> Result<
            Vec<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.list_all().await
        }

        async fn list_expired(
            &self,
            now_secs: i64,
        ) -> Result<
            Vec<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.list_expired(now_secs).await
        }

        async fn add_allotment(
            &self,
            mac: &str,
            metric: &str,
            amount: u64,
            now: i64,
        ) -> Result<merchant::CustomerSession, crate::v1_compat::session_store::SessionStoreError>
        {
            self.inner.add_allotment(mac, metric, amount, now).await
        }

        async fn record_external_usage(
            &self,
            mac: &str,
            snapshot: merchant::ExternalUsageSnapshot,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.record_external_usage(mac, snapshot).await
        }

        async fn remove_expired(
            &self,
            mac: &str,
            now_secs: i64,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.remove_expired(mac, now_secs).await
        }

        async fn remove_exhausted_bytes(
            &self,
            mac: &str,
            usage: u64,
        ) -> Result<
            Option<merchant::CustomerSession>,
            crate::v1_compat::session_store::SessionStoreError,
        > {
            self.inner.remove_exhausted_bytes(mac, usage).await
        }
    }

    fn test_driver_accepting(expected_token: String) -> Driver {
        let identity = Arc::new(Identity::load_or_generate(&Config::default()).unwrap());
        let wallet = BootstrapWallet::new(vec![]).with_verify_override(move |token: String| {
            // clone per call: the boxed future must own its captures
            let expected = expected_token.clone();
            async move {
                assert_eq!(
                    token, expected,
                    "wallet.verify must see the submitted token"
                );
                Ok(1_000) // 1 sat in milli-sat
            }
        });
        Driver::new(
            wallet,
            IpAdapter::new(),
            identity,
            Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn regression_pr122_header_payment_credits_store_before_journal_completion() {
        // PR #145's audit gap 1 (#122/#131): an ACCEPTED header-identified
        // (delegated) payment must credit the session-API store via
        // credit_for_token — the split-ledger bug credited only the
        // driver. Acceptance requires wallet.verify to succeed (CDK
        // receive or NUT-07 checkstate — both live-mint HTTP), so the
        // wallet's cfg(test) verify override stands in for the mint;
        // everything above it (driver admit, BootstrapAck, the
        // handler's credit branch, journal bracketing) is production
        // code.
        let dir = std::env::temp_dir().join(format!("tg-hdr-credit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let journal_path = dir.join("pending.jsonl");

        let token = crate::wallet::token_with_mint("https://mint.example"); // 1 sat
        let probe = Arc::new(CreditOrderingProbe::new(&journal_path));
        let driver = test_driver_accepting(token.clone());
        let app = build_router(
            driver.clone(),
            test_config_with_journal(Some(journal_path.clone())),
            Some(probe.clone()),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "text/plain")
                    .header("x-tollgate-mac", "02:50:00:00:00:42")
                    .body(Body::from(token.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "an accepted delegated payment answers 200"
        );

        // The store row: the delegated MAC holds a 60_000 ms session
        // (1 sat at 1 sat/step, step 60_000 ms).
        let row = probe
            .inner
            .get("02:50:00:00:00:42")
            .await
            .unwrap()
            .expect("store row exists for the delegated MAC");
        assert_eq!(row.allotment, 60_000);
        assert_eq!(row.metric, "milliseconds");

        // The credit call: exactly one, for this MAC and token.
        {
            let credits = probe.credits.lock().unwrap();
            assert_eq!(credits.len(), 1, "exactly one store credit: {credits:?}");
            assert_eq!(
                credits[0],
                (
                    "02:50:00:00:00:42".to_owned(),
                    "milliseconds".to_owned(),
                    60_000,
                    token.clone()
                )
            );
        }

        // The token-claim: a second credit for the same token is a no-op
        // (crash-recovery re-derivation must not double-credit).
        assert!(
            probe
                .inner
                .credit_for_token(
                    "02:50:00:00:00:42",
                    "milliseconds",
                    60_000,
                    &token,
                    now_secs()
                )
                .await
                .unwrap()
                .is_none()
        );

        // The #129/#131 ordering: at credit time the journal STILL held
        // the entry (the pre-#131 shape completed the journal first,
        // making a crash between completion and credit unrecoverable).
        assert_eq!(
            probe
                .journal_held_token_at_credit
                .lock()
                .unwrap()
                .as_slice(),
            &[true],
            "credit must land BEFORE the journal entry is completed"
        );

        // Both ledgers: the driver also admitted the peer.
        let peer_hex = adapter::mac_to_peer_hex("02:50:00:00:00:42");
        let status = driver.status().await;
        let peer = status
            .peers
            .iter()
            .find(|p| p.pubkey == peer_hex)
            .expect("driver holds the admitted peer");
        assert_eq!(peer.state, "Active");

        // Journal cleanup: after the response the entry is gone.
        let journal = std::fs::read_to_string(&journal_path).unwrap();
        assert!(
            !journal.contains(&token),
            "completed entry removed from journal: {journal}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
