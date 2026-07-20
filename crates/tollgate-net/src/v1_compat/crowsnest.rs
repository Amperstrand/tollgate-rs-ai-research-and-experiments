//! Background auto-discovery loop for upstream TollGate gateways.
//!
//! The crowsnest is a polling-based background task that probes a configured
//! list of gateway IPs for upstream TollGate routers. When a new TollGate is
//! discovered, it hands off to a [`GatewayHandler`] for session creation.
//! When probing fails consistently, it triggers session teardown.
//!
//! This is the Rust equivalent of Go v1's `crowsnest` module, simplified to
//! use polling over configured IPs rather than event-driven network interface
//! monitoring (which is platform-specific and left for future work).

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use nostr::prelude::*;

use tokio::task::JoinHandle;

use super::nostr::TollGateAdvertisement;

// ---------------------------------------------------------------------------
// Type aliases for object-safe async trait
// ---------------------------------------------------------------------------

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default scan interval (30 seconds).
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Default probe timeout per gateway (5 seconds).
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of consecutive probe failures before triggering a disconnect.
const DISCONNECT_AFTER_CONSECUTIVE_FAILURES: u32 = 3;

// ---------------------------------------------------------------------------
// CrowsnestConfig
// ---------------------------------------------------------------------------

/// Configuration for the crowsnest auto-discovery loop.
#[derive(Debug, Clone)]
pub struct CrowsnestConfig {
    /// Gateway IPs to probe (static config; dynamic interface scanning is future work).
    pub gateway_ips: Vec<String>,
    /// How often to scan for new gateways.
    pub scan_interval: Duration,
    /// Probe timeout per gateway.
    pub probe_timeout: Duration,
    /// Require valid Nostr signature on advertisement.
    pub verify_signature: bool,
    /// Network interface name to report (for session manager).
    pub interface_name: String,
    /// MAC address to report (for session manager).
    pub mac_address: String,
}

impl Default for CrowsnestConfig {
    fn default() -> Self {
        Self {
            gateway_ips: Vec::new(),
            scan_interval: DEFAULT_SCAN_INTERVAL,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            verify_signature: true,
            interface_name: "eth0".to_owned(),
            mac_address: "00:00:00:00:00:00".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// GatewayHandler trait (replaces experimental SessionManager)
// ---------------------------------------------------------------------------

/// Error type for gateway handler operations.
#[derive(Debug, thiserror::Error)]
pub enum GatewayHandlerError {
    /// A session already exists for this gateway.
    #[error("session already exists for gateway {0}")]
    SessionExists(String),
    /// Another error occurred.
    #[error("{0}")]
    Other(String),
}

/// Trait for handling upstream gateway discovery and disconnect events.
///
/// In the original v1 code this was `SessionManager<W>`. In the v1-compat
/// layer, the concrete session manager is not yet ported, so this trait
/// decouples the crowsnest from session lifecycle management.
///
/// Methods return boxed futures so the trait is object-safe and can be
/// used as `Arc<dyn GatewayHandler>`.
pub trait GatewayHandler: Send + Sync {
    /// Called when a TollGate gateway is discovered.
    ///
    /// Returns `Err(GatewayHandlerError::SessionExists)` if a session for
    /// this gateway already exists (non-fatal).
    fn handle_gateway_connected(
        &self,
        interface_name: &str,
        mac_address: &str,
        gateway_ip: &str,
    ) -> BoxFuture<'_, Result<(), GatewayHandlerError>>;

    /// Called when a gateway should be disconnected (e.g., after repeated
    /// probe failures).
    fn handle_disconnect(
        &self,
        interface_name: &str,
    ) -> BoxFuture<'_, Result<(), GatewayHandlerError>>;
}

// ---------------------------------------------------------------------------
// Inlined upstream detection types (from v1 server/upstream_detector)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Inlined upstream detection functions (from v1 server/upstream_detector)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Crowsnest
// ---------------------------------------------------------------------------

/// Background auto-discovery loop that probes for upstream TollGate gateways.
///
/// Generic over `S: GatewayHandler` to allow different session backends.
/// Uses a `tokio::sync::watch` channel for cancellation instead of
/// `tokio_util::sync::CancellationToken` (which is not available in the
/// v1-compat dependency set).
pub struct Crowsnest {
    config: CrowsnestConfig,
    handler: Arc<dyn GatewayHandler>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
}

impl Crowsnest {
    /// Create a new crowsnest with the given config and shared gateway handler.
    pub fn new(config: CrowsnestConfig, handler: Arc<dyn GatewayHandler>) -> Self {
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        Self {
            config,
            handler,
            cancel_tx,
            cancel_rx,
        }
    }

    /// Get a cancel handle that can be used to stop the background task
    /// after it has been spawned.
    ///
    /// Send `true` to signal shutdown.
    pub fn cancel_handle(&self) -> tokio::sync::watch::Sender<bool> {
        self.cancel_tx.clone()
    }

    /// Spawn the background discovery loop and return the join handle.
    ///
    /// The loop runs until the cancellation channel receives `true`.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    /// Run the discovery loop (called by `spawn`).
    async fn run(self) {
        let Self {
            config,
            handler,
            cancel_tx: _,
            mut cancel_rx,
        } = self;

        tracing::info!(
            gateways = ?config.gateway_ips,
            scan_interval_secs = config.scan_interval.as_secs(),
            "Crowsnest started"
        );

        // Track consecutive probe failures per gateway IP.
        let mut failure_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_err() || *cancel_rx.borrow() {
                        tracing::info!("Crowsnest shutting down");
                        return;
                    }
                }
                () = tokio::time::sleep(config.scan_interval) => {}
            }

            if config.gateway_ips.is_empty() {
                continue;
            }

            for gateway_ip in &config.gateway_ips {
                let probe_result =
                    probe_gateway(gateway_ip, config.probe_timeout, config.verify_signature).await;

                match probe_result {
                    Ok(Some(_discovered)) => {
                        // Gateway is a TollGate — try to create a session.
                        // SessionExists is fine (already have a session).
                        // Reset failure count on success.
                        failure_counts.insert(gateway_ip.clone(), 0);

                        match handler
                            .handle_gateway_connected(
                                &config.interface_name,
                                &config.mac_address,
                                gateway_ip,
                            )
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    %gateway_ip,
                                    "Crowsnest created session for discovered gateway"
                                );
                            }
                            Err(GatewayHandlerError::SessionExists(_)) => {
                                tracing::debug!(
                                    %gateway_ip,
                                    "Crowsnest: session already exists for gateway"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    %gateway_ip,
                                    %e,
                                    "Crowsnest: failed to create session for discovered gateway"
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        // Gateway responded but is not a TollGate — skip.
                        tracing::debug!(
                            %gateway_ip,
                            "Crowsnest: gateway responded but is not a TollGate"
                        );
                        // Not a TollGate, but not a failure either — reset count.
                        failure_counts.insert(gateway_ip.clone(), 0);
                    }
                    Err(e) => {
                        // Probe failed — increment failure count.
                        let count = failure_counts.entry(gateway_ip.clone()).or_insert(0);
                        *count += 1;

                        tracing::debug!(
                            %gateway_ip,
                            %e,
                            failures = *count,
                            "Crowsnest: probe failed for gateway"
                        );

                        if *count >= DISCONNECT_AFTER_CONSECUTIVE_FAILURES {
                            tracing::warn!(
                                %gateway_ip,
                                failures = *count,
                                "Crowsnest: gateway unreachable after consecutive failures, disconnecting"
                            );
                            if let Err(e) = handler.handle_disconnect(&config.interface_name).await
                            {
                                tracing::warn!(
                                    %gateway_ip,
                                    %e,
                                    "Crowsnest: failed to disconnect sessions for interface"
                                );
                            }
                            // Reset to avoid repeatedly disconnecting.
                            *count = 0;
                        }
                    }
                }
            }
        }
    }
}
