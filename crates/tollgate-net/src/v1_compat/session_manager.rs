//! Multi-gateway session manager for v1 client mode.
//!
//! Matches Go v1's `UpstreamSessionManager`:
//! - Manages multiple upstream sessions by gateway IP
//! - Creates sessions when gateways are discovered
//! - Stops sessions when interfaces disconnect
//! - Handles renewal requests from usage trackers
//!
//! Works standalone with HTTP + Cashu tokens. Uses concrete
//! [`CdkWallet`](super::wallet::CdkWallet) — no experimental
//! `tollgate_core` trait dependencies.

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::client::{V1Client, V1ClientConfig, V1ClientError, V1Session};
use super::crowsnest::{GatewayHandler, GatewayHandlerError};
use super::http_client::TollGateHttpClient;
use super::usage_tracker::{RenewalRequest, UsageTrackerConfig, UsageTrackerHandle};
use super::wallet::CdkWallet;

// ---------------------------------------------------------------------------
// Session manager
// ---------------------------------------------------------------------------

/// Snapshot of an active upstream session's state.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Gateway IP address.
    pub gateway_ip: String,
    /// Interface name (e.g. `"eth0"`).
    pub interface_name: String,
    /// Total allotment from the upstream session.
    pub total_allotment: u64,
    /// Metric type (`"milliseconds"` or `"bytes"`).
    pub metric: String,
    /// Total sats spent on this gateway.
    pub total_spent_sats: u64,
    /// Number of payments made.
    pub payment_count: u32,
    /// How long ago the session was created.
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
}

/// State for a single upstream session.
pub struct UpstreamSessionState {
    /// Gateway IP.
    pub gateway_ip: String,
    /// Interface name.
    pub interface_name: String,
    /// V1 client managing this upstream connection.
    pub client: V1Client,
    /// Handle to the background usage tracker task.
    pub tracker_handle: Option<UsageTrackerHandle>,
    /// Cancellation token for this session.
    pub cancel: CancellationToken,
    /// When the session was created.
    pub created_at: std::time::Instant,
    /// When the last payment was made.
    pub last_payment_at: Option<std::time::Instant>,
    /// Total sats spent on this gateway.
    pub total_spent_sats: u64,
    /// Number of payments made.
    pub payment_count: u32,
}

/// Configuration for the session manager.
pub struct SessionManagerConfig {
    /// V1 client configuration template.
    pub client_config: V1ClientConfig,
    /// Usage tracker configuration.
    pub tracker_config: UsageTrackerConfig,
}

/// Multi-gateway session manager.
///
/// Owns the renewal channel and a map of gateway IP → session state.
/// External callers invoke `handle_gateway_connected` and `handle_disconnect`
/// to manage session lifecycle.
pub struct SessionManager {
    /// Manager configuration.
    pub config: SessionManagerConfig,
    /// Shared CDK wallet for Cashu operations.
    pub wallet: Arc<CdkWallet>,
    /// Active sessions keyed by gateway IP.
    pub sessions: RwLock<HashMap<String, UpstreamSessionState>>,
    renewal_rx: RwLock<tokio::sync::mpsc::Receiver<RenewalRequest>>,
    /// Sender for renewal requests (shared with usage trackers).
    pub renewal_tx: tokio::sync::mpsc::Sender<RenewalRequest>,
    cancel: CancellationToken,
}

impl SessionManager {
    /// Create a new session manager with the given config and shared wallet.
    #[must_use]
    pub fn new(config: SessionManagerConfig, wallet: Arc<CdkWallet>) -> Self {
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
    ///
    /// # Errors
    ///
    /// Returns `SessionManagerError::SessionExists` if a session for this
    /// gateway already exists, or `SessionManagerError::Client` if connection
    /// fails.
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

        let mut client = V1Client::new(client_config);
        client.connect(&self.wallet).await?;

        let payment_count = u32::from(client.session().is_some());

        let cancel = CancellationToken::new();
        let http = TollGateHttpClient::new(gateway_ip);

        let tracker_handle = super::usage_tracker::spawn_usage_tracker(
            http,
            self.config.tracker_config.clone(),
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
    ///
    /// # Errors
    ///
    /// Currently always returns `Ok`; errors are reserved for future use.
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
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` when cancelled or the channel closes.
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

// ---------------------------------------------------------------------------
// GatewayHandler trait impl
// ---------------------------------------------------------------------------

impl GatewayHandler for SessionManager {
    fn handle_gateway_connected(
        &self,
        interface_name: &str,
        mac_address: &str,
        gateway_ip: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GatewayHandlerError>> + Send + '_>> {
        let interface_name = interface_name.to_owned();
        let mac_address = mac_address.to_owned();
        let gateway_ip = gateway_ip.to_owned();
        Box::pin(async move {
            SessionManager::handle_gateway_connected(
                self,
                &interface_name,
                &mac_address,
                &gateway_ip,
            )
            .await
            .map_err(|e| match e {
                SessionManagerError::SessionExists(ip) => GatewayHandlerError::SessionExists(ip),
                other => GatewayHandlerError::Other(other.to_string()),
            })
        })
    }

    fn handle_disconnect(
        &self,
        interface_name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), GatewayHandlerError>> + Send + '_>> {
        let interface_name = interface_name.to_owned();
        Box::pin(async move {
            SessionManager::handle_disconnect(self, &interface_name)
                .await
                .map_err(|e| GatewayHandlerError::Other(e.to_string()))
        })
    }
}
