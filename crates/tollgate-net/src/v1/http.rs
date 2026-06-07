//! HTTP client for v1 TollGate protocol (TIP-03).
//!
//! Communicates with upstream TollGate routers on port 2121:
//! - `GET /` → Fetch advertisement (kind 10021)
//! - `POST /` → Send Cashu token, receive session (kind 1022) or notice (kind 21023)
//! - `GET /usage` → Poll usage ("usage/allotment" or "-1/-1")

use reqwest::Client;
use std::time::Duration;

use super::nostr_events::{NoticeEvent, SessionEvent, TollGateAdvertisement, V1NostrError};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct LnInvoiceRequest {
    pub amount: u64,
    pub mint_url: Option<String>,
    pub mint: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct LnInvoiceResponse {
    pub status: u8,
    pub quote: Option<String>,
    pub invoice: Option<String>,
    pub mint_url: Option<String>,
    pub amount: Option<u64>,
    pub expiry: Option<u64>,
    pub state: String,
    pub access_granted: bool,
    pub allotment: Option<u64>,
    pub metric: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct LnInvoiceStatus {
    pub quote: Option<String>,
    pub state: String,
    pub access_granted: bool,
    pub allotment: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum V1HttpError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("nostr event error: {0}")]
    Nostr(#[from] V1NostrError),
    #[error("upstream rejected payment: {code} - {message}")]
    PaymentRejected { code: String, message: String },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

/// HTTP client for a single upstream TollGate.
pub struct TollGateHttpClient {
    pub client: Client,
    pub base_url: String,
    pub probe_retry_count: u32,
    pub probe_retry_delay: Duration,
}

impl TollGateHttpClient {
    /// Create a client targeting `gateway_ip:2121`.
    pub fn new(gateway_ip: &str) -> Self {
        Self::new_with_base_url(&format!("http://{gateway_ip}:2121"))
    }

    /// Create a client with an explicit base URL (for testing).
    pub fn new_with_base_url(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client construction should not fail");
        Self {
            client,
            base_url: base_url.to_owned(),
            probe_retry_count: 3,
            probe_retry_delay: Duration::from_secs(2),
        }
    }

    /// Fetch the TollGate advertisement (`GET /`) with retry.
    pub async fn fetch_advertisement(&self) -> Result<TollGateAdvertisement, V1HttpError> {
        let mut last_err = None;
        for attempt in 0..self.probe_retry_count {
            if attempt > 0 {
                tracing::info!(
                    attempt,
                    total = self.probe_retry_count,
                    "Retrying tollgate probe"
                );
                tokio::time::sleep(self.probe_retry_delay).await;
            }
            match self.fetch_advertisement_once().await {
                Ok(ad) => return Ok(ad),
                Err(e) => {
                    tracing::debug!(attempt, error = %e, "Probe attempt failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }

    /// Single attempt to fetch the TollGate advertisement.
    async fn fetch_advertisement_once(&self) -> Result<TollGateAdvertisement, V1HttpError> {
        let response = self
            .client
            .get(&self.base_url)
            .send()
            .await?
            .error_for_status()?;

        let body = response.text().await?;
        let ad = TollGateAdvertisement::from_json(&body)?;
        tracing::info!(
            pubkey = %ad.pubkey(),
            metric = ?ad.metric(),
            options = ad.pricing_options().len(),
            "Fetched TollGate advertisement"
        );
        Ok(ad)
    }

    /// Send a Cashu token as plain text (`POST /`).
    ///
    /// The v1 Go server accepts plain Cashu token strings directly
    /// (no Nostr event wrapping required — TIP-03).
    /// Returns a SessionEvent on success or describes the rejection.
    pub async fn send_payment(&self, cashu_token: &str) -> Result<SessionEvent, V1HttpError> {
        let response = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "text/plain")
            .body(cashu_token.to_owned())
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if status == reqwest::StatusCode::OK {
            let session = SessionEvent::from_json(&body)?;
            tracing::info!(
                allotment = ?session.allotment(),
                metric = ?session.metric(),
                "Payment accepted, session created"
            );
            return Ok(session);
        }

        // Try to parse as notice event for structured error info
        if let Ok(notice) = NoticeEvent::from_json(&body) {
            return Err(V1HttpError::PaymentRejected {
                code: notice.code().unwrap_or_else(|| "unknown".into()),
                message: notice.message().to_owned(),
            });
        }

        Err(V1HttpError::Unexpected(format!("status {status}: {body}")))
    }

    /// Poll current usage (`GET /usage`).
    ///
    /// Returns `(usage, allotment)`. `(0, 0)` means no session.
    /// `(-1, -1)` from server is mapped to `(0, 0)`.
    pub async fn fetch_usage(&self) -> Result<(i64, i64), V1HttpError> {
        let url = format!("{}/usage", self.base_url);
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body = response.text().await?;

        let (usage, allotment) = parse_usage_response(&body);
        tracing::debug!(usage, allotment, "Fetched usage");
        Ok((usage, allotment))
    }

    /// Request a Lightning invoice from the upstream TollGate (`POST /ln-invoice`).
    ///
    /// The server creates a NUT-04 mint quote and returns a BOLT11 invoice
    /// that the client must pay externally. Once paid, the server mints
    /// tokens and grants access.
    pub async fn request_ln_invoice(&self, amount: u64, mint_url: &str) -> Result<LnInvoiceResponse, V1HttpError> {
        let request = LnInvoiceRequest {
            amount,
            mint_url: Some(mint_url.to_owned()),
            mint: None,
        };
        let response = self
            .client
            .post(&format!("{}/ln-invoice", self.base_url))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let body = response.text().await?;
        let resp: LnInvoiceResponse = serde_json::from_str(&body)?;
        tracing::debug!(
            quote = ?resp.quote,
            state = %resp.state,
            "Received Lightning invoice"
        );
        Ok(resp)
    }

    /// Poll Lightning invoice status (`GET /ln-invoice?quote=xxx`).
    ///
    /// Returns the current quote state (UNPAID/PAID/ISSUED) and whether
    /// access has been granted.
    pub async fn check_ln_invoice_status(&self, quote: &str) -> Result<LnInvoiceStatus, V1HttpError> {
        let url = format!("{}/ln-invoice?quote={}", self.base_url, quote);
        let response = self.client.get(&url).send().await?.error_for_status()?;
        let body = response.text().await?;

        let status: LnInvoiceStatus = serde_json::from_str(&body)?;
        tracing::debug!(
            state = %status.state,
            access_granted = status.access_granted,
            "Checked Lightning invoice status"
        );
        Ok(status)
    }
}

/// Parse "usage/allotment" response from `GET /usage`.
///
/// Format: `"12345/60000"` or `"-1/-1"` for no session.
fn parse_usage_response(body: &str) -> (i64, i64) {
    let trimmed = body.trim();
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 2 {
        let usage: i64 = parts[0].parse().unwrap_or(-1);
        let allotment: i64 = parts[1].parse().unwrap_or(-1);
        // Map -1/-1 to 0/0 (no session)
        if usage < 0 || allotment < 0 {
            return (0, 0);
        }
        (usage, allotment)
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use nostr::event::tag::{Tag, TagKind};
    use nostr::prelude::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn parse_usage_normal() {
        assert_eq!(parse_usage_response("12345/60000"), (12345, 60000));
    }

    #[test]
    fn parse_usage_no_session() {
        assert_eq!(parse_usage_response("-1/-1"), (0, 0));
    }

    #[test]
    fn parse_usage_zero() {
        assert_eq!(parse_usage_response("0/60000"), (0, 60000));
    }

    #[test]
    fn parse_usage_whitespace() {
        assert_eq!(parse_usage_response("  100/200  \n"), (100, 200));
    }

    #[test]
    fn parse_usage_garbage() {
        assert_eq!(parse_usage_response("not a number"), (0, 0));
    }

    struct RetryServerState {
        fail_count: u32,
        request_count: u32,
        keys: Keys,
    }

    impl RetryServerState {
        fn new(fail_count: u32) -> Self {
            Self {
                fail_count,
                request_count: 0,
                keys: Keys::generate(),
            }
        }
    }

    async fn retry_advertisement(
        State(state): State<Arc<Mutex<RetryServerState>>>,
    ) -> impl IntoResponse {
        let mut s = state.lock().expect("lock");
        s.request_count += 1;
        if s.request_count <= s.fail_count {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let tags = Tags::from_list(vec![
            Tag::custom(
                TagKind::Custom("metric".into()),
                ["milliseconds".to_owned()],
            ),
            Tag::custom(TagKind::Custom("step_size".into()), ["60000".to_owned()]),
            Tag::custom(
                TagKind::Custom("price_per_step".into()),
                [
                    "cashu".to_owned(),
                    "1".to_owned(),
                    "sat".to_owned(),
                    "https://testnut.cashu.exchange".to_owned(),
                    "1".to_owned(),
                ],
            ),
        ]);
        let event = EventBuilder::new(Kind::Custom(10_021), "")
            .tags(tags)
            .sign_with_keys(&s.keys)
            .expect("sign ad event");
        axum::Json(serde_json::to_value(event).expect("serialize ad event")).into_response()
    }

    async fn start_retry_server(
        state: Arc<Mutex<RetryServerState>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let app = Router::new()
            .route("/", get(retry_advertisement))
            .with_state(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server error");
        });
        (base_url, handle)
    }

    fn make_retry_client(
        base_url: &str,
        retry_count: u32,
        retry_delay: Duration,
    ) -> TollGateHttpClient {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client construction should not fail");
        TollGateHttpClient {
            client,
            base_url: base_url.to_owned(),
            probe_retry_count: retry_count,
            probe_retry_delay: retry_delay,
        }
    }

    #[tokio::test]
    async fn fetch_advertisement_retries_on_failure_then_succeeds() {
        let state = Arc::new(Mutex::new(RetryServerState::new(2)));
        let (base_url, server) = start_retry_server(state.clone()).await;

        let http = make_retry_client(&base_url, 3, Duration::from_millis(10));
        let result = http.fetch_advertisement().await;
        assert!(result.is_ok(), "should succeed after retries: {result:?}");

        let requests = state.lock().expect("lock").request_count;
        assert_eq!(
            requests, 3,
            "should make 3 attempts (2 failures + 1 success)"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn fetch_advertisement_returns_last_error_when_exhausted() {
        let state = Arc::new(Mutex::new(RetryServerState::new(99)));
        let (base_url, server) = start_retry_server(state.clone()).await;

        let http = make_retry_client(&base_url, 2, Duration::from_millis(10));
        let result = http.fetch_advertisement().await;
        assert!(result.is_err(), "should fail when all attempts exhausted");

        let requests = state.lock().expect("lock").request_count;
        assert_eq!(requests, 2, "should make exactly 2 attempts");

        server.abort();
        let _ = server.await;
    }
}
