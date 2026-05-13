//! Multi-gateway session manager for v1 client mode.
//!
//! Matches Go v1's `UpstreamSessionManager`:
//! - Manages multiple upstream sessions by gateway IP
//! - Creates sessions when gateways are discovered
//! - Stops sessions when interfaces disconnect
//! - Handles renewal requests from usage trackers

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tollgate_core::wallet::Wallet;

use super::http::TollGateHttpClient;
use super::usage_tracker::{RenewalRequest, UsageTrackerConfig, UsageTrackerHandle};
use super::{V1Client, V1ClientConfig, V1ClientError, V1Session};

/// Snapshot of an active upstream session's state.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub gateway_ip: String,
    pub interface_name: String,
    pub total_allotment: u64,
    pub metric: String,
    pub total_spent_sats: u64,
    pub payment_count: u32,
    pub created_at_ago: std::time::Duration,
}

/// Error type for session manager operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("session already exists for gateway {0}")]
    SessionExists(String),
    #[error("no session for gateway {0}")]
    NoSession(String),
    #[error("client error: {0}")]
    Client(#[from] V1ClientError),
    #[error("wallet error: {0}")]
    Wallet(#[from] tollgate_core::error::WalletError),
}

/// State for a single upstream session.
pub struct UpstreamSessionState<W: Wallet> {
    pub gateway_ip: String,
    pub interface_name: String,
    pub client: V1Client<W>,
    pub tracker_handle: Option<UsageTrackerHandle>,
    pub cancel: CancellationToken,
    pub created_at: std::time::Instant,
    pub last_payment_at: Option<std::time::Instant>,
    pub total_spent_sats: u64,
    pub payment_count: u32,
}

/// Configuration for the session manager.
pub struct SessionManagerConfig {
    pub client_config: V1ClientConfig,
    pub tracker_config: UsageTrackerConfig,
}

/// Multi-gateway session manager.
///
/// Owns the renewal channel and a map of gateway IP → session state.
/// External callers invoke `handle_gateway_connected` and `handle_disconnect`
/// to manage session lifecycle.
pub struct SessionManager<W: Wallet> {
    config: SessionManagerConfig,
    wallet: Arc<W>,
    sessions: RwLock<HashMap<String, UpstreamSessionState<W>>>,
    renewal_rx: RwLock<tokio::sync::mpsc::Receiver<RenewalRequest>>,
    renewal_tx: tokio::sync::mpsc::Sender<RenewalRequest>,
    cancel: CancellationToken,
}

impl<W: Wallet + 'static> SessionManager<W> {
    /// Create a new session manager with the given config and shared wallet.
    pub fn new(config: SessionManagerConfig, wallet: Arc<W>) -> Self {
        let (renewal_tx, renewal_rx) = tokio::sync::mpsc::channel(64);
        Self {
            config,
            wallet,
            sessions: RwLock::new(HashMap::new()),
            renewal_rx: RwLock::new(renewal_rx),
            renewal_tx,
            cancel: CancellationToken::new(),
        }
    }

    /// Handle a new gateway connection.
    ///
    /// Creates a V1Client, connects to the upstream, spawns a usage tracker,
    /// and stores the session. Returns error if a session already exists for
    /// this gateway IP.
    pub async fn handle_gateway_connected(
        &self,
        interface_name: &str,
        _mac_address: &str,
        gateway_ip: &str,
    ) -> Result<(), SessionManagerError> {
        {
            let sessions = self.sessions.read().await;
            if sessions.contains_key(gateway_ip) {
                return Err(SessionManagerError::SessionExists(gateway_ip.to_owned()));
            }
        }

        let mut client_config = self.config.client_config.clone();
        client_config.gateway_ip = gateway_ip.to_owned();

        let mut client = V1Client::<W>::new(client_config);
        client.connect(&self.wallet).await?;

        let payment_count = u32::from(client.session().is_some());

        let cancel = CancellationToken::new();
        let http = TollGateHttpClient::new(gateway_ip);

        let tracker_handle = super::usage_tracker::spawn_usage_tracker(
            http,
            self.config.tracker_config.clone(),
            self.wallet.clone(),
            self.renewal_tx.clone(),
            gateway_ip.to_owned(),
            cancel.clone(),
        );

        let now = std::time::Instant::now();
        let state = UpstreamSessionState {
            gateway_ip: gateway_ip.to_owned(),
            interface_name: interface_name.to_owned(),
            client,
            tracker_handle: Some(tracker_handle),
            cancel,
            created_at: now,
            last_payment_at: Some(now),
            total_spent_sats: 0,
            payment_count,
        };

        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(gateway_ip.to_owned(), state);
        }

        tracing::info!(
            %gateway_ip,
            %interface_name,
            "Session created for gateway"
        );

        Ok(())
    }

    /// Handle interface disconnection — stop all sessions on the given interface.
    pub async fn handle_disconnect(&self, interface_name: &str) -> Result<(), SessionManagerError> {
        let mut sessions = self.sessions.write().await;
        let gateway_ips: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.interface_name == interface_name)
            .map(|(ip, _)| ip.clone())
            .collect();

        for ip in &gateway_ips {
            if let Some(session) = sessions.remove(ip) {
                session.cancel.cancel();
                if let Some(handle) = session.tracker_handle {
                    handle.join_handle.abort();
                }
                tracing::info!(
                    gateway_ip = %ip,
                    %interface_name,
                    "Session removed for disconnected interface"
                );
            }
        }

        Ok(())
    }

    /// Return a snapshot of all active sessions.
    pub async fn get_active_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .map(|s| {
                let (allotment, metric) = match s.client.session() {
                    Some(V1Session {
                        total_allotment,
                        metric,
                        ..
                    }) => (*total_allotment, metric.clone()),
                    None => (0, String::new()),
                };

                SessionInfo {
                    gateway_ip: s.gateway_ip.clone(),
                    interface_name: s.interface_name.clone(),
                    total_allotment: allotment,
                    metric,
                    total_spent_sats: s.total_spent_sats,
                    payment_count: s.payment_count,
                    created_at_ago: s.created_at.elapsed(),
                }
            })
            .collect()
    }

    /// Shutdown all sessions.
    pub async fn stop(&self) {
        self.cancel.cancel();
        let mut sessions = self.sessions.write().await;
        for (ip, session) in sessions.drain() {
            session.cancel.cancel();
            if let Some(handle) = session.tracker_handle {
                handle.join_handle.abort();
            }
            tracing::info!(gateway_ip = %ip, "Session stopped");
        }
    }

    /// Main loop: process renewal requests until cancelled.
    ///
    /// Reads `RenewalRequest` messages from the internal channel and calls
    /// `renew()` on the matching session's `V1Client`.
    pub async fn run(&self) -> Result<(), SessionManagerError> {
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    tracing::info!("Session manager shutting down");
                    return Ok(());
                }
                request = async {
                    let mut rx = self.renewal_rx.write().await;
                    rx.recv().await
                } => {
                    if let Some(req) = request {
                        self.process_renewal(&req).await;
                    } else {
                        tracing::warn!("Renewal channel closed, shutting down");
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Process a single renewal request.
    async fn process_renewal(&self, req: &RenewalRequest) {
        let mut sessions = self.sessions.write().await;
        let Some(session) = sessions.get_mut(&req.gateway_ip) else {
            tracing::warn!(
                gateway_ip = %req.gateway_ip,
                "Renewal request for unknown gateway"
            );
            return;
        };

        if req.current_allotment == 0 {
            tracing::info!(
                gateway_ip = %req.gateway_ip,
                "No active session, reconnecting"
            );
            match session.client.connect(&self.wallet).await {
                Ok(()) => {
                    session.payment_count += 1;
                    session.last_payment_at = Some(std::time::Instant::now());
                    tracing::info!(gateway_ip = %req.gateway_ip, "Reconnected successfully");
                }
                Err(e) => {
                    tracing::error!(gateway_ip = %req.gateway_ip, error = %e, "Reconnect failed");
                }
            }
            return;
        }

        tracing::info!(
            gateway_ip = %req.gateway_ip,
            usage = req.current_usage,
            allotment = req.current_allotment,
            "Processing renewal"
        );

        match session.client.renew(&self.wallet).await {
            Ok(()) => {
                session.payment_count += 1;
                session.last_payment_at = Some(std::time::Instant::now());
                tracing::info!(gateway_ip = %req.gateway_ip, "Renewal completed");
            }
            Err(e) => {
                tracing::error!(gateway_ip = %req.gateway_ip, error = %e, "Renewal failed");
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

    struct MockServerState {
        payments_received: u64,
        usage: u64,
        allotment: u64,
        keys: Keys,
    }

    impl MockServerState {
        fn new(keys: Keys) -> Self {
            Self {
                payments_received: 0,
                usage: 0,
                allotment: 0,
                keys,
            }
        }
    }

    async fn get_advertisement(
        State(state): State<Arc<Mutex<MockServerState>>>,
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

    async fn post_payment(State(state): State<Arc<Mutex<MockServerState>>>) -> impl IntoResponse {
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

    async fn get_server_usage(
        State(state): State<Arc<Mutex<MockServerState>>>,
    ) -> impl IntoResponse {
        let mut s = state.lock().expect("lock");
        s.usage += 5000;
        format!("{}/{}", s.usage, s.allotment)
    }

    fn session_test_app(state: Arc<Mutex<MockServerState>>) -> Router {
        Router::new()
            .route("/", get(get_advertisement).post(post_payment))
            .route("/usage", get(get_server_usage))
            .with_state(state)
    }

    async fn start_session_test_server(
        state: Arc<Mutex<MockServerState>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let app = session_test_app(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server error");
        });
        (base_url, handle)
    }

    fn make_session_config(gateway_ip: &str) -> V1ClientConfig {
        V1ClientConfig {
            gateway_ip: gateway_ip.to_owned(),
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

    fn make_manager_config(gateway_ip: &str) -> SessionManagerConfig {
        SessionManagerConfig {
            client_config: make_session_config(gateway_ip),
            tracker_config: UsageTrackerConfig {
                poll_interval: std::time::Duration::from_millis(50),
                renewal_threshold: 0.8,
            },
        }
    }

    #[tokio::test]
    async fn session_manager_creates_session_for_gateway() {
        let keys = Keys::generate();
        let state = Arc::new(Mutex::new(MockServerState::new(keys)));
        let (base_url, server) = start_session_test_server(state.clone()).await;

        let wallet = Arc::new(MockWallet::new(1000));
        let mut config = make_manager_config("10.0.0.1");
        config.client_config.gateway_ip = "10.0.0.1".to_owned();

        let manager = SessionManager::new(config, wallet);

        // We need to override the HTTP client to point at the test server.
        // Since V1Client::new uses gateway_ip to construct the URL, we need
        // to manually construct the client with the test URL.
        let client_config = make_session_config("10.0.0.1");
        let mut client = V1Client::<MockWallet>::new_with_base_url(client_config, &base_url);
        client
            .connect(&manager.wallet)
            .await
            .expect("connect should succeed");

        let cancel = CancellationToken::new();
        let tracker_cancel = CancellationToken::new();
        let http = TollGateHttpClient::new_with_base_url(&base_url);

        let tracker_handle = super::super::usage_tracker::spawn_usage_tracker(
            http,
            manager.config.tracker_config.clone(),
            manager.wallet.clone(),
            manager.renewal_tx.clone(),
            "10.0.0.1".to_owned(),
            tracker_cancel.clone(),
        );

        let session_state = UpstreamSessionState {
            gateway_ip: "10.0.0.1".to_owned(),
            interface_name: "eth0".to_owned(),
            client,
            tracker_handle: Some(tracker_handle),
            cancel,
            created_at: std::time::Instant::now(),
            last_payment_at: Some(std::time::Instant::now()),
            total_spent_sats: 1,
            payment_count: 1,
        };

        {
            let mut sessions = manager.sessions.write().await;
            sessions.insert("10.0.0.1".to_owned(), session_state);
        }

        let active = manager.get_active_sessions().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].gateway_ip, "10.0.0.1");
        assert_eq!(active[0].interface_name, "eth0");
        assert!(active[0].total_allotment > 0);
        assert_eq!(active[0].payment_count, 1);

        manager.stop().await;
        tracker_cancel.cancel();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn session_manager_rejects_duplicate_session() {
        let keys = Keys::generate();
        let state = Arc::new(Mutex::new(MockServerState::new(keys)));
        let (base_url, server) = start_session_test_server(state.clone()).await;

        let wallet = Arc::new(MockWallet::new(1000));
        let mut config = make_manager_config("10.0.0.1");
        config.client_config.gateway_ip = "10.0.0.1".to_owned();

        let manager = SessionManager::new(config, wallet);

        // Manually insert a session to simulate existing state.
        let mut client =
            V1Client::<MockWallet>::new_with_base_url(make_session_config("10.0.0.1"), &base_url);
        client.connect(&manager.wallet).await.expect("connect");

        let cancel = CancellationToken::new();
        let session_state = UpstreamSessionState {
            gateway_ip: "10.0.0.1".to_owned(),
            interface_name: "eth0".to_owned(),
            client,
            tracker_handle: None,
            cancel,
            created_at: std::time::Instant::now(),
            last_payment_at: None,
            total_spent_sats: 0,
            payment_count: 0,
        };

        {
            let mut sessions = manager.sessions.write().await;
            sessions.insert("10.0.0.1".to_owned(), session_state);
        }

        // Now try to connect the same gateway via handle_gateway_connected.
        let result = manager
            .handle_gateway_connected("eth0", "00:11:22:33:44:55", "10.0.0.1")
            .await;
        match result {
            Err(SessionManagerError::SessionExists(ref ip)) if ip == "10.0.0.1" => {}
            other => panic!("should reject duplicate with SessionExists: {other:?}"),
        }

        manager.stop().await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn session_manager_disconnect_removes_sessions() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config("10.0.0.1");

        let manager = SessionManager::new(config, wallet);

        // Start a separate server for each gateway.
        let mut server_handles = Vec::new();
        for (iface, gw_ip) in [("eth0", "10.0.0.1"), ("wlan0", "10.0.0.2")] {
            let keys = Keys::generate();
            let state = Arc::new(Mutex::new(MockServerState::new(keys)));
            let (base_url, server) = start_session_test_server(state.clone()).await;
            server_handles.push(server);

            let mut client =
                V1Client::<MockWallet>::new_with_base_url(make_session_config(gw_ip), &base_url);
            client.connect(&manager.wallet).await.expect("connect");

            let session_state = UpstreamSessionState {
                gateway_ip: gw_ip.to_owned(),
                interface_name: iface.to_owned(),
                client,
                tracker_handle: None,
                cancel: CancellationToken::new(),
                created_at: std::time::Instant::now(),
                last_payment_at: None,
                total_spent_sats: 0,
                payment_count: 0,
            };

            let mut sessions = manager.sessions.write().await;
            sessions.insert(gw_ip.to_owned(), session_state);
        }

        assert_eq!(manager.get_active_sessions().await.len(), 2);

        // Disconnect eth0 — only that session should be removed.
        manager.handle_disconnect("eth0").await.expect("disconnect");
        let active = manager.get_active_sessions().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].gateway_ip, "10.0.0.2");
        assert_eq!(active[0].interface_name, "wlan0");

        manager.stop().await;
        for s in server_handles {
            s.abort();
            let _ = s.await;
        }
    }

    #[tokio::test]
    async fn session_manager_stop_cleans_up_all_sessions() {
        let wallet = Arc::new(MockWallet::new(1000));
        let config = make_manager_config("10.0.0.1");

        let manager = SessionManager::new(config, wallet);

        let mut server_handles = Vec::new();
        for (iface, gw_ip) in [("eth0", "10.0.0.1"), ("wlan0", "10.0.0.2")] {
            let keys = Keys::generate();
            let state = Arc::new(Mutex::new(MockServerState::new(keys)));
            let (base_url, server) = start_session_test_server(state.clone()).await;
            server_handles.push(server);

            let mut client =
                V1Client::<MockWallet>::new_with_base_url(make_session_config(gw_ip), &base_url);
            client.connect(&manager.wallet).await.expect("connect");

            let session_state = UpstreamSessionState {
                gateway_ip: gw_ip.to_owned(),
                interface_name: iface.to_owned(),
                client,
                tracker_handle: None,
                cancel: CancellationToken::new(),
                created_at: std::time::Instant::now(),
                last_payment_at: None,
                total_spent_sats: 0,
                payment_count: 0,
            };

            let mut sessions = manager.sessions.write().await;
            sessions.insert(gw_ip.to_owned(), session_state);
        }

        assert_eq!(manager.get_active_sessions().await.len(), 2);
        manager.stop().await;
        assert_eq!(manager.get_active_sessions().await.len(), 0);

        for s in server_handles {
            s.abort();
            let _ = s.await;
        }
    }

    #[tokio::test]
    async fn session_manager_process_renewal_renews_session() {
        let keys = Keys::generate();
        let state = Arc::new(Mutex::new(MockServerState::new(keys)));
        let (base_url, server) = start_session_test_server(state.clone()).await;

        let wallet = Arc::new(MockWallet::new(10000));
        let config = make_manager_config("10.0.0.1");

        let manager = SessionManager::new(config, wallet);

        let mut client =
            V1Client::<MockWallet>::new_with_base_url(make_session_config("10.0.0.1"), &base_url);
        client.connect(&manager.wallet).await.expect("connect");

        let initial_allotment = client.session().unwrap().total_allotment;

        let session_state = UpstreamSessionState {
            gateway_ip: "10.0.0.1".to_owned(),
            interface_name: "eth0".to_owned(),
            client,
            tracker_handle: None,
            cancel: CancellationToken::new(),
            created_at: std::time::Instant::now(),
            last_payment_at: None,
            total_spent_sats: 0,
            payment_count: 1,
        };

        {
            let mut sessions = manager.sessions.write().await;
            sessions.insert("10.0.0.1".to_owned(), session_state);
        }

        let req = RenewalRequest {
            gateway_ip: "10.0.0.1".to_owned(),
            current_usage: 50_000,
            current_allotment: 60_000,
        };

        manager.process_renewal(&req).await;

        let sessions = manager.sessions.read().await;
        let session = sessions.get("10.0.0.1").expect("session should exist");
        assert_eq!(session.payment_count, 2);
        let renewed_allotment = session.client.session().unwrap().total_allotment;
        assert!(
            renewed_allotment > initial_allotment,
            "allotment should increase: {renewed_allotment} vs {initial_allotment}"
        );

        drop(sessions);
        manager.stop().await;
        server.abort();
        let _ = server.await;
    }
}
