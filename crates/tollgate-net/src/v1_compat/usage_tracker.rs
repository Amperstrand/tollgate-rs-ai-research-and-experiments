//! Background usage tracker for upstream TollGate sessions.
//!
//! Polls `/usage` at a configurable interval and sends a renewal request
//! when usage reaches a configurable threshold of the allotment.
//!
//! Matches Go v1's `upstream_usage_tracker.go` behavior:
//! - Polls `/usage` via [`TollGateHttpClient::fetch_usage`](super::http_client::TollGateHttpClient)
//! - Tracks allotment changes (new session, session expired, renewal completed)
//! - When usage reaches threshold, sends `RenewalRequest` via channel
//! - Handles `(0, 0)` (no session) by triggering initial payment request
//! - Throttles renewal requests: minimum 10 seconds between requests

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::http_client::TollGateHttpClient;

// ---------------------------------------------------------------------------
// Usage tracker
// ---------------------------------------------------------------------------

/// Minimum time between renewal requests (prevents renewal storms).
const MIN_RENEWAL_INTERVAL: Duration = Duration::from_secs(10);

/// Log at info level every N polls.
const INFO_LOG_INTERVAL: u32 = 5;

/// Configuration for the usage tracker.
#[derive(Debug, Clone)]
pub struct UsageTrackerConfig {
    /// How often to poll `/usage`.
    pub poll_interval: Duration,
    /// Renew when usage reaches this fraction of allotment (0.0–1.0).
    pub renewal_threshold: f64,
}

impl Default for UsageTrackerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            renewal_threshold: 0.8,
        }
    }
}

/// A request from a usage tracker that a session needs renewal.
#[derive(Debug, Clone)]
pub struct RenewalRequest {
    /// Gateway IP of the upstream needing renewal.
    pub gateway_ip: String,
    /// Current usage value from the last poll.
    pub current_usage: u64,
    /// Current allotment value from the last poll.
    pub current_allotment: u64,
}

/// Handle to a running usage tracker task.
pub struct UsageTrackerHandle {
    /// Token to cancel the tracker task.
    pub cancel: CancellationToken,
    /// Join handle for the spawned task.
    pub join_handle: tokio::task::JoinHandle<()>,
}

/// Spawn a background usage tracker for a single upstream session.
///
/// The tracker polls `/usage` at `poll_interval` and sends a `RenewalRequest`
/// via the `renewal_callback` channel when usage reaches `renewal_threshold`
/// of allotment, or when no session exists (usage=0, allotment=0).
///
/// Returns a handle for cancellation.
#[must_use]
pub fn spawn_usage_tracker(
    http_client: TollGateHttpClient,
    config: UsageTrackerConfig,
    renewal_callback: tokio::sync::mpsc::Sender<RenewalRequest>,
    gateway_ip: String,
    cancel: CancellationToken,
) -> UsageTrackerHandle {
    let join_handle = tokio::spawn(async move {
        run_tracker(http_client, config, renewal_callback, gateway_ip, cancel).await;
    });

    UsageTrackerHandle {
        cancel: CancellationToken::new(),
        join_handle,
    }
}

/// Core tracker loop.
async fn run_tracker(
    http_client: TollGateHttpClient,
    config: UsageTrackerConfig,
    renewal_tx: tokio::sync::mpsc::Sender<RenewalRequest>,
    gateway_ip: String,
    cancel: CancellationToken,
) {
    let mut poll_count: u32 = 0;
    let mut last_renewal_request: Option<std::time::Instant> = None;

    tracing::info!(%gateway_ip, "Usage tracker started");

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!(%gateway_ip, "Usage tracker cancelled");
                return;
            }
            () = tokio::time::sleep(config.poll_interval) => {}
        }

        poll_count += 1;

        let (usage, allotment) = match http_client.fetch_usage().await {
            Ok((u, a)) => (u.max(0) as u64, a.max(0) as u64),
            Err(e) => {
                tracing::debug!(%gateway_ip, error = %e, "Usage poll failed");
                continue;
            }
        };

        if poll_count % INFO_LOG_INTERVAL == 0 {
            tracing::info!(
                %gateway_ip,
                usage,
                allotment,
                poll_count,
                "Usage tracker poll"
            );
        } else {
            tracing::debug!(
                %gateway_ip,
                usage,
                allotment,
                poll_count,
                "Usage tracker poll"
            );
        }

        let needs_request = if allotment == 0 {
            tracing::info!(%gateway_ip, "No session detected, requesting initial payment");
            true
        } else {
            let ratio = usage as f64 / allotment as f64;
            ratio >= config.renewal_threshold
        };

        if needs_request {
            // Throttle: don't send requests more often than MIN_RENEWAL_INTERVAL.
            if let Some(last) = last_renewal_request {
                if last.elapsed() < MIN_RENEWAL_INTERVAL {
                    tracing::debug!(
                        %gateway_ip,
                        elapsed_ms = last.elapsed().as_millis(),
                        "Throttling renewal request"
                    );
                    continue;
                }
            }

            let request = RenewalRequest {
                gateway_ip: gateway_ip.clone(),
                current_usage: usage,
                current_allotment: allotment,
            };

            match renewal_tx.send(request).await {
                Ok(()) => {
                    tracing::info!(
                        %gateway_ip,
                        usage,
                        allotment,
                        "Sent renewal request"
                    );
                    last_renewal_request = Some(std::time::Instant::now());
                }
                Err(e) => {
                    tracing::warn!(
                        %gateway_ip,
                        error = %e,
                        "Failed to send renewal request (channel closed)"
                    );
                    return;
                }
            }
        }
    }
}
