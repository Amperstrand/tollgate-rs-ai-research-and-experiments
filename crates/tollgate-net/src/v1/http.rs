//! HTTP client for v1 TollGate protocol (TIP-03).
//!
//! Communicates with upstream TollGate routers on port 2121:
//! - `GET /` → Fetch advertisement (kind 10021)
//! - `POST /` → Send Cashu token, receive session (kind 1022) or notice (kind 21023)
//! - `GET /usage` → Poll usage ("usage/allotment" or "-1/-1")

use reqwest::Client;
use std::time::Duration;

use super::nostr_events::{NoticeEvent, SessionEvent, TollGateAdvertisement, V1NostrError};

#[derive(Debug, thiserror::Error)]
pub enum V1HttpError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("nostr event error: {0}")]
    Nostr(#[from] V1NostrError),
    #[error("upstream rejected payment: {code} - {message}")]
    PaymentRejected { code: String, message: String },
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

/// HTTP client for a single upstream TollGate.
pub struct TollGateHttpClient {
    client: Client,
    base_url: String,
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
        Self { client, base_url: base_url.to_owned() }
    }

    /// Fetch the TollGate advertisement (`GET /`).
    pub async fn fetch_advertisement(&self) -> Result<TollGateAdvertisement, V1HttpError> {
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
    pub async fn send_payment(
        &self,
        cashu_token: &str,
    ) -> Result<SessionEvent, V1HttpError> {
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

        Err(V1HttpError::Unexpected(format!(
            "status {status}: {body}"
        )))
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
}
