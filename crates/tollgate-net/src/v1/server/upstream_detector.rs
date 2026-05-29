#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

//! Upstream TollGate detector.
//!
//! Probes network gateways for upstream TollGate routers by making HTTP
//! requests to gateway IPs on port 2121. When an upstream TollGate is
//! found, the response is parsed as a Nostr kind 10021 advertisement event.
//!
//! This matches Go v1's `crowsnest`/`upstream_detector` pattern, providing
//! the probe and parse building blocks. The background scanning task
//! (which depends on platform-specific network interface enumeration) is
//! left for future work.

use std::time::Duration;

use nostr::prelude::*;

use crate::v1::nostr_events::TollGateAdvertisement;

/// Default probe timeout (5 seconds).
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default scan interval (30 seconds).
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Configuration for the upstream detector.
#[derive(Debug, Clone)]
pub struct UpstreamDetectorConfig {
    /// Interfaces to ignore (e.g., `["lo", "br-lan", "hostap0"]`).
    pub ignore_interfaces: Vec<String>,
    /// Probe timeout per gateway.
    pub probe_timeout: Duration,
    /// How often to scan for new gateways.
    pub scan_interval: Duration,
    /// Require valid Nostr signature on advertisement.
    pub require_valid_signature: bool,
}

impl Default for UpstreamDetectorConfig {
    fn default() -> Self {
        Self {
            ignore_interfaces: vec!["lo".to_owned()],
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            scan_interval: DEFAULT_SCAN_INTERVAL,
            require_valid_signature: true,
        }
    }
}

/// A discovered upstream TollGate.
#[derive(Debug, Clone)]
pub struct DiscoveredUpstream {
    /// Gateway IP of the upstream TollGate.
    pub gateway_ip: String,
    /// Network interface via which the gateway was reached.
    pub interface: String,
    /// Nostr public key of the upstream TollGate operator.
    pub nostr_pubkey: String,
    /// Metric type (e.g., `"bytes"`, `"milliseconds"`).
    pub metric: String,
    /// Step size in metric units.
    pub step_size: u64,
    /// Accepted mints with pricing.
    pub accepted_mints: Vec<UpstreamMint>,
}

/// An accepted mint from an upstream advertisement.
#[derive(Debug, Clone)]
pub struct UpstreamMint {
    /// Mint URL.
    pub url: String,
    /// Price per step in the given unit.
    pub price_per_step: u64,
    /// Currency unit (e.g., `"sat"`).
    pub unit: String,
    /// Minimum steps required per purchase.
    pub min_steps: u64,
}

/// Error type for upstream detection.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamDetectError {
    /// HTTP request failed or returned non-200.
    #[error("HTTP error: {0}")]
    Http(String),
    /// Advertisement JSON could not be parsed or is invalid.
    #[error("invalid advertisement: {0}")]
    InvalidAdvertisement(String),
    /// Nostr signature verification failed.
    #[error("signature verification failed")]
    InvalidSignature,
    /// Probe timed out.
    #[error("probe timeout")]
    Timeout,
}

/// Parse a Nostr kind 10021 advertisement event into [`DiscoveredUpstream`].
///
/// The `gateway_ip` and `interface` fields are set from caller context
/// (they are not part of the Nostr event itself).
///
/// If `verify_signature` is `true`, the Nostr event signature is checked
/// and [`UpstreamDetectError::InvalidSignature`] is returned on failure.
pub fn parse_advertisement(
    event_json: &str,
    gateway_ip: &str,
    interface: &str,
    verify_signature: bool,
) -> Result<DiscoveredUpstream, UpstreamDetectError> {
    let ad = TollGateAdvertisement::from_json(event_json).map_err(|e| {
        UpstreamDetectError::InvalidAdvertisement(format!("failed to parse event: {e}"))
    })?;

    if verify_signature && !ad.event.verify_signature() {
        return Err(UpstreamDetectError::InvalidSignature);
    }

    let metric = ad
        .metric()
        .ok_or_else(|| UpstreamDetectError::InvalidAdvertisement("missing 'metric' tag".into()))?;

    let step_size = ad.step_size().ok_or_else(|| {
        UpstreamDetectError::InvalidAdvertisement("missing or invalid 'step_size' tag".into())
    })?;

    let pricing = ad.pricing_options();
    if pricing.is_empty() {
        return Err(UpstreamDetectError::InvalidAdvertisement(
            "no 'price_per_step' tags found".into(),
        ));
    }

    let accepted_mints = pricing
        .into_iter()
        .map(|p| UpstreamMint {
            url: p.mint_url,
            price_per_step: p.price_per_step,
            unit: p.unit,
            min_steps: p.min_steps,
        })
        .collect();

    Ok(DiscoveredUpstream {
        gateway_ip: gateway_ip.to_owned(),
        interface: interface.to_owned(),
        nostr_pubkey: ad.event.pubkey.to_hex(),
        metric,
        step_size,
        accepted_mints,
    })
}

/// Probe a single gateway IP for an upstream TollGate.
///
/// Sends `GET http://{gateway_ip}:2121/` and parses the response body as a
/// Nostr kind 10021 advertisement event. Returns `Ok(Some(...))` if a valid
/// TollGate is found, `Ok(None)` if the gateway responded but is not a
/// TollGate, or an error if the probe itself failed.
pub async fn probe_gateway(
    gateway_ip: &str,
    timeout: Duration,
    verify_signature: bool,
) -> Result<Option<DiscoveredUpstream>, UpstreamDetectError> {
    let url = format!("http://{gateway_ip}:2121/");
    probe_url(&url, gateway_ip, timeout, verify_signature).await
}

/// Probe an arbitrary URL for an upstream TollGate.
///
/// Like [`probe_gateway`] but accepts a full URL instead of assuming port 2121.
/// Useful for testing or non-standard deployments.
pub async fn probe_url(
    url: &str,
    gateway_ip: &str,
    timeout: Duration,
    verify_signature: bool,
) -> Result<Option<DiscoveredUpstream>, UpstreamDetectError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| UpstreamDetectError::Http(format!("failed to build HTTP client: {e}")))?;

    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            UpstreamDetectError::Timeout
        } else {
            UpstreamDetectError::Http(format!("request failed: {e}"))
        }
    })?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let body = response
        .text()
        .await
        .map_err(|e| UpstreamDetectError::Http(format!("failed to read response body: {e}")))?;

    match parse_advertisement(&body, gateway_ip, "unknown", verify_signature) {
        Ok(discovered) => Ok(Some(discovered)),
        Err(UpstreamDetectError::InvalidAdvertisement(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::event::tag::{Tag, TagKind};

    fn build_advertisement_event(
        keys: &Keys,
        metric: &str,
        step_size: u64,
        mints: &[(&str, u64, &str, &str, u64)],
    ) -> String {
        let mut tags: Vec<Tag> = vec![
            Tag::custom(TagKind::Custom("metric".into()), [metric.to_owned()]),
            Tag::custom(TagKind::Custom("step_size".into()), [step_size.to_string()]),
        ];

        for &(_asset, price, unit, url, min_steps) in mints {
            tags.push(Tag::custom(
                TagKind::Custom("price_per_step".into()),
                [
                    "cashu".to_owned(),
                    price.to_string(),
                    unit.to_owned(),
                    url.to_owned(),
                    min_steps.to_string(),
                ],
            ));
        }

        let event = EventBuilder::new(Kind::Custom(10_021), "")
            .tags(Tags::from_list(tags))
            .sign_with_keys(keys)
            .unwrap();

        event.as_json()
    }

    #[test]
    fn parse_valid_advertisement() {
        let keys = Keys::generate();
        let json = build_advertisement_event(
            &keys,
            "bytes",
            22_020_096,
            &[
                ("cashu", 1, "sat", "https://mint.example.com", 0),
                ("cashu", 2, "sat", "https://mint2.example.com", 5),
            ],
        );

        let result = parse_advertisement(&json, "192.168.1.1", "eth0", true).unwrap();

        assert_eq!(result.gateway_ip, "192.168.1.1");
        assert_eq!(result.interface, "eth0");
        assert_eq!(result.nostr_pubkey, keys.public_key().to_hex());
        assert_eq!(result.metric, "bytes");
        assert_eq!(result.step_size, 22_020_096);
        assert_eq!(result.accepted_mints.len(), 2);

        assert_eq!(result.accepted_mints[0].url, "https://mint.example.com");
        assert_eq!(result.accepted_mints[0].price_per_step, 1);
        assert_eq!(result.accepted_mints[0].unit, "sat");
        assert_eq!(result.accepted_mints[0].min_steps, 0);

        assert_eq!(result.accepted_mints[1].url, "https://mint2.example.com");
        assert_eq!(result.accepted_mints[1].price_per_step, 2);
        assert_eq!(result.accepted_mints[1].min_steps, 5);
    }

    #[test]
    fn parse_wrong_kind_rejected() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1022), "test")
            .sign_with_keys(&keys)
            .unwrap();
        let json = event.as_json();

        let result = parse_advertisement(&json, "10.0.0.1", "wlan0", true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse event"),
            "expected parse error, got: {err}"
        );
    }

    #[test]
    fn parse_invalid_signature_rejected() {
        let keys = Keys::generate();
        let mut json = build_advertisement_event(
            &keys,
            "milliseconds",
            60000,
            &[("cashu", 1, "sat", "https://mint.example.com", 0)],
        );

        // Tamper with the signature by flipping a character.
        // The JSON contains "sig": "<hex>", so find "sig" and corrupt the value.
        if let Some(idx) = json.find("\"sig\":\"") {
            let sig_start = idx + 7;
            if sig_start < json.len() {
                let c = json.chars().nth(sig_start).unwrap();
                let flipped = if c == 'a' { 'b' } else { 'a' };
                json = format!(
                    "{}{}{}",
                    &json[..sig_start],
                    flipped,
                    &json[sig_start + flipped.len_utf8()..]
                );
            }
        }

        let result = parse_advertisement(&json, "10.0.0.1", "wlan0", true);
        assert!(
            matches!(result, Err(UpstreamDetectError::InvalidSignature)),
            "expected InvalidSignature, got: {result:?}"
        );
    }

    #[test]
    fn parse_missing_tags_rejected() {
        let keys = Keys::generate();
        // Build event with no pricing tags — should fail.
        let event = EventBuilder::new(Kind::Custom(10_021), "")
            .tags(Tags::from_list(vec![
                Tag::custom(TagKind::Custom("metric".into()), ["bytes".to_owned()]),
                Tag::custom(TagKind::Custom("step_size".into()), ["1000".to_owned()]),
            ]))
            .sign_with_keys(&keys)
            .unwrap();
        let json = event.as_json();

        let result = parse_advertisement(&json, "10.0.0.1", "wlan0", true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no 'price_per_step' tags found"),
            "expected missing tags error, got: {err}"
        );
    }

    #[test]
    fn parse_missing_metric_rejected() {
        let keys = Keys::generate();
        // Build event with pricing but no metric tag.
        let event = EventBuilder::new(Kind::Custom(10_021), "")
            .tags(Tags::from_list(vec![
                Tag::custom(TagKind::Custom("step_size".into()), ["1000".to_owned()]),
                Tag::custom(
                    TagKind::Custom("price_per_step".into()),
                    [
                        "cashu".to_owned(),
                        "1".to_owned(),
                        "sat".to_owned(),
                        "https://mint.example.com".to_owned(),
                        "0".to_owned(),
                    ],
                ),
            ]))
            .sign_with_keys(&keys)
            .unwrap();
        let json = event.as_json();

        let result = parse_advertisement(&json, "10.0.0.1", "wlan0", true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("missing 'metric' tag"),
            "expected missing metric error, got: {err}"
        );
    }

    #[test]
    fn parse_skip_signature_check() {
        let keys = Keys::generate();
        let mut json = build_advertisement_event(
            &keys,
            "bytes",
            1000,
            &[("cashu", 1, "sat", "https://mint.example.com", 0)],
        );

        // Tamper the signature.
        if let Some(idx) = json.find("\"sig\":\"") {
            let sig_start = idx + 7;
            if sig_start < json.len() {
                let c = json.chars().nth(sig_start).unwrap();
                let flipped = if c == 'a' { 'b' } else { 'a' };
                json = format!(
                    "{}{}{}",
                    &json[..sig_start],
                    flipped,
                    &json[sig_start + flipped.len_utf8()..]
                );
            }
        }

        // With verify_signature=false, should succeed despite tampered sig.
        let result = parse_advertisement(&json, "10.0.0.1", "wlan0", false);
        assert!(result.is_ok(), "expected Ok with sig check disabled");
    }

    #[test]
    fn config_default_values() {
        let config = UpstreamDetectorConfig::default();
        assert_eq!(config.ignore_interfaces, vec!["lo"]);
        assert_eq!(config.probe_timeout, DEFAULT_PROBE_TIMEOUT);
        assert_eq!(config.scan_interval, DEFAULT_SCAN_INTERVAL);
        assert!(config.require_valid_signature);
    }

    #[tokio::test]
    async fn probe_gateway_timeout() {
        // Use a non-routable IP to guarantee timeout.
        // 198.51.100.1 is in TEST-NET-2 (RFC 5737) — will never respond.
        let result = probe_gateway("198.51.100.1", Duration::from_millis(50), true).await;

        match result {
            Err(UpstreamDetectError::Timeout) => {} // expected
            Err(UpstreamDetectError::Http(msg)) => {
                // Connection refused or network unreachable also acceptable
                // since the IP might not route at all on some systems.
                assert!(
                    msg.contains("request failed"),
                    "unexpected HTTP error: {msg}"
                );
            }
            other => {
                panic!("expected Timeout or Http error, got: {other:?}");
            }
        }
    }

    #[tokio::test]
    async fn probe_gateway_with_local_server() {
        use tokio::io::AsyncWriteExt;

        // Start a local HTTP server that returns a valid advertisement.
        let keys = Keys::generate();
        let ad_json = build_advertisement_event(
            &keys,
            "bytes",
            1000,
            &[("cashu", 5, "sat", "https://mint.test", 1)],
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = ad_json;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            // Keep connection alive briefly so client can read.
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let result = probe_url(
            &format!("http://127.0.0.1:{port}/"),
            "127.0.0.1",
            Duration::from_secs(2),
            true,
        )
        .await;

        server_handle.await.unwrap();

        let discovered = result
            .expect("probe should succeed")
            .expect("should find a TollGate");

        assert_eq!(discovered.metric, "bytes");
        assert_eq!(discovered.step_size, 1000);
        assert_eq!(discovered.accepted_mints.len(), 1);
        assert_eq!(discovered.accepted_mints[0].url, "https://mint.test");
        assert_eq!(discovered.accepted_mints[0].price_per_step, 5);
        assert_eq!(discovered.accepted_mints[0].unit, "sat");
        assert_eq!(discovered.accepted_mints[0].min_steps, 1);
    }

    #[tokio::test]
    async fn probe_gateway_non_tollgate_returns_none() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = r#"{"status": "ok"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let result = probe_url(
            &format!("http://127.0.0.1:{port}/"),
            "127.0.0.1",
            Duration::from_secs(2),
            true,
        )
        .await;

        server_handle.await.unwrap();

        assert!(
            result.unwrap().is_none(),
            "non-TollGate response should return None"
        );
    }
}
