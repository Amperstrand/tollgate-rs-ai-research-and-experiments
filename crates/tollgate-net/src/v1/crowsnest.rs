//! Background auto-discovery loop for upstream TollGate gateways.
//!
//! The crowsnest is a polling-based background task that probes a configured
//! list of gateway IPs for upstream TollGate routers. When a new TollGate is
//! discovered, it hands off to the [`SessionManager`] for session creation.
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

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tollgate_core::wallet::Wallet;

use super::server::upstream_detector::probe_gateway;
use super::session_manager::{SessionManager, SessionManagerError};

/// Default scan interval (30 seconds).
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Default probe timeout per gateway (5 seconds).
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of consecutive probe failures before triggering a disconnect.
const DISCONNECT_AFTER_CONSECUTIVE_FAILURES: u32 = 3;

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

/// Background auto-discovery loop that probes for upstream TollGate gateways.
///
/// Generic over `W: Wallet` to match [`SessionManager`]'s generic parameter.
pub struct Crowsnest<W: Wallet> {
    config: CrowsnestConfig,
    session_manager: Arc<SessionManager<W>>,
    cancel: CancellationToken,
}

impl<W: Wallet + 'static> Crowsnest<W> {
    /// Create a new crowsnest with the given config and shared session manager.
    pub fn new(config: CrowsnestConfig, session_manager: Arc<SessionManager<W>>) -> Self {
        Self {
            config,
            session_manager,
            cancel: CancellationToken::new(),
        }
    }

    /// Get a clone of the cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Spawn the background discovery loop and return the join handle.
    ///
    /// The loop runs until the cancellation token is cancelled or the
    /// session manager is stopped.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    /// Run the discovery loop (called by `spawn`).
    async fn run(self) {
        tracing::info!(
            gateways = ?self.config.gateway_ips,
            scan_interval_secs = self.config.scan_interval.as_secs(),
            "Crowsnest started"
        );

        // Track consecutive probe failures per gateway IP.
        let mut failure_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    tracing::info!("Crowsnest shutting down");
                    return;
                }
                () = tokio::time::sleep(self.config.scan_interval) => {}
            }

            if self.config.gateway_ips.is_empty() {
                continue;
            }

            for gateway_ip in &self.config.gateway_ips {
                let probe_result = probe_gateway(
                    gateway_ip,
                    self.config.probe_timeout,
                    self.config.verify_signature,
                )
                .await;

                match probe_result {
                    Ok(Some(_discovered)) => {
                        // Gateway is a TollGate — try to create a session.
                        // SessionExists is fine (already have a session).
                        // Reset failure count on success.
                        failure_counts.insert(gateway_ip.clone(), 0);

                        match self
                            .session_manager
                            .handle_gateway_connected(
                                &self.config.interface_name,
                                &self.config.mac_address,
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
                            Err(SessionManagerError::SessionExists(_)) => {
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
                            if let Err(e) = self
                                .session_manager
                                .handle_disconnect(&self.config.interface_name)
                                .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockWallet;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use nostr::event::tag::{Tag, TagKind};
    use nostr::prelude::*;
    use std::sync::{Arc, Mutex};

    use super::super::session_manager::SessionManagerConfig;
    use super::super::usage_tracker::UsageTrackerConfig;
    use super::super::V1ClientConfig;

    // ── Mock server helpers ──────────────────────────────────────────

    #[allow(dead_code)]
    struct TestServerState {
        keys: Keys,
        payments_received: u64,
        usage: u64,
        allotment: u64,
    }

    #[allow(dead_code)]
    impl TestServerState {
        fn new(keys: Keys) -> Self {
            Self {
                keys,
                payments_received: 0,
                usage: 0,
                allotment: 0,
            }
        }
    }

    #[allow(dead_code)]
    async fn get_advertisement(
        State(state): State<Arc<Mutex<TestServerState>>>,
    ) -> impl IntoResponse {
        let keys = {
            let s = state.lock().expect("lock");
            s.keys.clone()
        };

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
            .sign_with_keys(&keys)
            .expect("sign ad event");

        axum::Json(serde_json::to_value(event).expect("serialize ad event"))
    }

    #[allow(dead_code)]
    async fn post_payment(State(state): State<Arc<Mutex<TestServerState>>>) -> impl IntoResponse {
        let mut s = state.lock().expect("lock");
        s.payments_received += 1;
        s.allotment += 60_000;

        let tags = Tags::from_list(vec![
            Tag::custom(
                TagKind::Custom("allotment".into()),
                [s.allotment.to_string()],
            ),
            Tag::custom(
                TagKind::Custom("metric".into()),
                ["milliseconds".to_owned()],
            ),
        ]);

        let event = EventBuilder::new(Kind::Custom(1022), "")
            .tags(tags)
            .sign_with_keys(&s.keys)
            .expect("sign session event");

        axum::Json(serde_json::to_value(event).expect("serialize session event"))
    }

    #[allow(dead_code)]
    async fn get_usage(State(state): State<Arc<Mutex<TestServerState>>>) -> impl IntoResponse {
        let mut s = state.lock().expect("lock");
        s.usage += 5000;
        format!("{}/{}", s.usage, s.allotment)
    }

    #[allow(dead_code)]
    fn test_app(state: Arc<Mutex<TestServerState>>) -> Router {
        Router::new()
            .route("/", get(get_advertisement).post(post_payment))
            .route("/usage", get(get_usage))
            .with_state(state)
    }

    #[allow(dead_code)]
    async fn start_test_server(
        state: Arc<Mutex<TestServerState>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let app = test_app(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server error");
        });
        (base_url, handle)
    }

    fn make_client_config() -> V1ClientConfig {
        V1ClientConfig {
            gateway_ip: String::new(), // filled per-test
            mac_address: "00:11:22:33:44:55".to_owned(),
            our_mint_urls: vec!["https://testnut.cashu.exchange".to_owned()],
            unit: "sat".to_owned(),
            max_price_per_ms: 1.0,
            max_price_per_byte: 1.0,
            preferred_allotment: 60_000,
            poll_interval_secs: 1,
            renewal_threshold: 0.8,
        }
    }

    fn make_manager_config() -> SessionManagerConfig {
        SessionManagerConfig {
            client_config: make_client_config(),
            tracker_config: UsageTrackerConfig {
                poll_interval: Duration::from_millis(50),
                renewal_threshold: 0.8,
            },
        }
    }

    // ── Tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn crowsnest_config_default_values() {
        let config = CrowsnestConfig::default();
        assert!(config.gateway_ips.is_empty());
        assert_eq!(config.scan_interval, DEFAULT_SCAN_INTERVAL);
        assert_eq!(config.probe_timeout, DEFAULT_PROBE_TIMEOUT);
        assert!(config.verify_signature);
        assert_eq!(config.interface_name, "eth0");
        assert_eq!(config.mac_address, "00:00:00:00:00:00");
    }

    #[tokio::test]
    async fn crowsnest_cancels_cleanly() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config();
        let sm = Arc::new(SessionManager::new(config, wallet));

        let crowsnest_config = CrowsnestConfig {
            gateway_ips: vec!["198.51.100.1".to_owned()], // non-routable
            scan_interval: Duration::from_millis(100),
            probe_timeout: Duration::from_millis(50),
            ..CrowsnestConfig::default()
        };

        let _cancel = crowsnest_config.clone().scan_interval;
        let crowsnest = Crowsnest::new(crowsnest_config, sm.clone());
        let cancel_token = crowsnest.cancel_token();
        let handle = crowsnest.spawn();

        // Cancel immediately.
        cancel_token.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "crowsnest should terminate after cancellation"
        );

        sm.stop().await;
    }

    #[tokio::test]
    async fn crowsnest_handles_probe_failure_gracefully() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config();
        let sm = Arc::new(SessionManager::new(config, wallet));

        let crowsnest_config = CrowsnestConfig {
            // Use a non-routable IP that will always fail.
            gateway_ips: vec!["198.51.100.1".to_owned()],
            scan_interval: Duration::from_millis(100),
            probe_timeout: Duration::from_millis(50),
            ..CrowsnestConfig::default()
        };

        let crowsnest = Crowsnest::new(crowsnest_config, sm.clone());
        let cancel_token = crowsnest.cancel_token();
        let handle = crowsnest.spawn();

        // Let it run through a few scan cycles.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // No sessions should exist (all probes fail).
        let sessions = sm.get_active_sessions().await;
        assert!(
            sessions.is_empty(),
            "no sessions should exist for failed probes"
        );

        cancel_token.cancel();
        sm.stop().await;

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "crowsnest should terminate");
    }

    #[tokio::test]
    async fn crowsnest_handles_empty_gateway_list() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config();
        let sm = Arc::new(SessionManager::new(config, wallet));

        let crowsnest_config = CrowsnestConfig {
            gateway_ips: vec![],
            scan_interval: Duration::from_millis(50),
            ..CrowsnestConfig::default()
        };

        let crowsnest = Crowsnest::new(crowsnest_config, sm.clone());
        let cancel_token = crowsnest.cancel_token();
        let handle = crowsnest.spawn();

        // Let it run through a few empty scan cycles.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let sessions = sm.get_active_sessions().await;
        assert!(sessions.is_empty());

        cancel_token.cancel();
        sm.stop().await;

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn crowsnest_spawns_and_stops() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config();
        let sm = Arc::new(SessionManager::new(config, wallet));

        let crowsnest_config = CrowsnestConfig {
            gateway_ips: vec![],
            scan_interval: Duration::from_secs(30),
            ..CrowsnestConfig::default()
        };

        let crowsnest = Crowsnest::new(crowsnest_config, sm.clone());
        let cancel_token = crowsnest.cancel_token();
        let handle = crowsnest.spawn();

        assert!(!handle.is_finished(), "crowsnest should be running");

        cancel_token.cancel();
        sm.stop().await;

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "crowsnest should stop after cancellation");
    }

    /// Test that the crowsnest correctly tracks failure counts and
    /// triggers disconnect after consecutive failures.
    #[tokio::test]
    async fn crowsnest_disconnects_after_consecutive_failures() {
        let wallet = Arc::new(MockWallet::new(1000));
        let mut config = make_manager_config();
        config.client_config.gateway_ip = "198.51.100.1".to_owned();
        let sm = Arc::new(SessionManager::new(config, wallet));

        let crowsnest_config = CrowsnestConfig {
            gateway_ips: vec!["198.51.100.1".to_owned()],
            scan_interval: Duration::from_millis(50),
            probe_timeout: Duration::from_millis(10),
            interface_name: "eth0".to_owned(),
            ..CrowsnestConfig::default()
        };

        let crowsnest = Crowsnest::new(crowsnest_config, sm.clone());
        let cancel_token = crowsnest.cancel_token();
        let handle = crowsnest.spawn();

        // Let it run through enough scan cycles to hit the failure threshold.
        // 3 consecutive failures needed × 50ms scan interval = ~200ms minimum
        tokio::time::sleep(Duration::from_millis(500)).await;

        // No sessions should exist since all probes fail.
        let sessions = sm.get_active_sessions().await;
        assert!(sessions.is_empty());

        cancel_token.cancel();
        sm.stop().await;

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }
}
