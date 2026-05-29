//! Background usage tracker for upstream TollGate sessions.
//!
//! Polls `/usage` at a configurable interval and sends a renewal request
//! when usage reaches a configurable threshold of the allotment.
//!
//! Matches Go v1's `upstream_usage_tracker.go` behavior:
//! - Polls `/usage` via `http_client.fetch_usage()`
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

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tollgate_core::wallet::Wallet;

use super::http::TollGateHttpClient;

/// Minimum time between renewal requests (prevents renewal storms).
const MIN_RENEWAL_INTERVAL: Duration = Duration::from_secs(10);

/// Log at info level every N polls.
const INFO_LOG_INTERVAL: u32 = 5;

/// Configuration for the usage tracker.
#[derive(Debug, Clone)]
pub struct UsageTrackerConfig {
    /// How often to poll /usage.
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
pub fn spawn_usage_tracker<W: Wallet + 'static>(
    http_client: TollGateHttpClient,
    config: UsageTrackerConfig,
    _wallet: Arc<W>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockWallet;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    /// Server state that returns configurable usage/allotment.
    struct TrackerServerState {
        usage: i64,
        allotment: i64,
        request_count: u32,
    }

    async fn get_tracker_usage(
        State(state): State<Arc<Mutex<TrackerServerState>>>,
    ) -> impl IntoResponse {
        let mut s = state.lock().expect("lock");
        s.request_count += 1;
        format!("{}/{}", s.usage, s.allotment)
    }

    fn tracker_app(state: Arc<Mutex<TrackerServerState>>) -> Router {
        Router::new()
            .route("/usage", get(get_tracker_usage))
            .with_state(state)
    }

    async fn start_tracker_server(
        state: Arc<Mutex<TrackerServerState>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let app = tracker_app(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server error");
        });
        (base_url, handle)
    }

    #[tokio::test]
    async fn tracker_polls_and_detects_renewal_threshold() {
        // allotment=60000, usage=50000 → ratio=0.83 > 0.8 threshold
        let state = Arc::new(Mutex::new(TrackerServerState {
            usage: 50_000,
            allotment: 60_000,
            request_count: 0,
        }));
        let (base_url, server) = start_tracker_server(state.clone()).await;

        let http_client = TollGateHttpClient::new_with_base_url(&base_url);
        let wallet = Arc::new(MockWallet::new(1000));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RenewalRequest>(10);
        let cancel = CancellationToken::new();

        let config = UsageTrackerConfig {
            poll_interval: Duration::from_millis(50),
            renewal_threshold: 0.8,
        };

        let _handle = spawn_usage_tracker(
            http_client,
            config,
            wallet,
            tx,
            "10.0.0.1".into(),
            cancel.clone(),
        );

        let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive renewal request within timeout")
            .expect("should have a request");

        assert_eq!(request.gateway_ip, "10.0.0.1");
        assert_eq!(request.current_usage, 50_000);
        assert_eq!(request.current_allotment, 60_000);

        cancel.cancel();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn tracker_sends_initial_payment_request_for_no_session() {
        // usage=0, allotment=0 means no session
        let state = Arc::new(Mutex::new(TrackerServerState {
            usage: 0,
            allotment: 0,
            request_count: 0,
        }));
        let (base_url, server) = start_tracker_server(state.clone()).await;

        let http_client = TollGateHttpClient::new_with_base_url(&base_url);
        let wallet = Arc::new(MockWallet::new(1000));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RenewalRequest>(10);
        let cancel = CancellationToken::new();

        let config = UsageTrackerConfig {
            poll_interval: Duration::from_millis(50),
            renewal_threshold: 0.8,
        };

        let _handle = spawn_usage_tracker(
            http_client,
            config,
            wallet,
            tx,
            "192.168.1.1".into(),
            cancel.clone(),
        );

        let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive initial payment request within timeout")
            .expect("should have a request");

        assert_eq!(request.gateway_ip, "192.168.1.1");
        assert_eq!(request.current_usage, 0);
        assert_eq!(request.current_allotment, 0);

        cancel.cancel();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn tracker_respects_cancellation() {
        let state = Arc::new(Mutex::new(TrackerServerState {
            usage: 50_000,
            allotment: 60_000,
            request_count: 0,
        }));
        let (base_url, server) = start_tracker_server(state.clone()).await;

        let http_client = TollGateHttpClient::new_with_base_url(&base_url);
        let wallet = Arc::new(MockWallet::new(1000));
        let (tx, _rx) = tokio::sync::mpsc::channel::<RenewalRequest>(10);
        let cancel = CancellationToken::new();

        let config = UsageTrackerConfig {
            poll_interval: Duration::from_millis(50),
            renewal_threshold: 0.8,
        };

        let handle = spawn_usage_tracker(
            http_client,
            config,
            wallet,
            tx,
            "10.0.0.1".into(),
            cancel.clone(),
        );

        // Cancel immediately.
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), handle.join_handle).await;
        assert!(
            result.is_ok(),
            "tracker should terminate after cancellation"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn tracker_throttles_rapid_renewal_requests() {
        // High usage ratio that always triggers renewal.
        let state = Arc::new(Mutex::new(TrackerServerState {
            usage: 55_000,
            allotment: 60_000,
            request_count: 0,
        }));
        let (base_url, server) = start_tracker_server(state.clone()).await;

        let http_client = TollGateHttpClient::new_with_base_url(&base_url);
        let wallet = Arc::new(MockWallet::new(1000));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RenewalRequest>(10);
        let cancel = CancellationToken::new();

        let config = UsageTrackerConfig {
            poll_interval: Duration::from_millis(50),
            renewal_threshold: 0.8,
        };

        let _handle = spawn_usage_tracker(
            http_client,
            config,
            wallet,
            tx,
            "10.0.0.1".into(),
            cancel.clone(),
        );

        // First request should arrive quickly.
        let _first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("first request timeout")
            .expect("first request");

        // Verify throttle: no second request within the 10s cooldown window.
        let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(
            result.is_err(),
            "should NOT receive a second renewal request within throttle window"
        );

        cancel.cancel();
        server.abort();
        let _ = server.await;
    }
}
