//! Stub network monitor for non-Linux platforms.
//!
//! Matches Go v1's non-Linux behavior: emits a fake `InterfaceUp` for `eth0`
//! after a 2-second delay, then does nothing. All other methods are no-ops.
//!
//! This module is used when:
//! - Target OS is not Linux, OR
//! - The `netlink` feature is not enabled

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// Re-export the same public types as the Linux module so consumers
// can use them regardless of platform.

/// Network event (stub — only `InterfaceUp` for eth0 is ever emitted).
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    InterfaceUp {
        name: String,
        gateway_ip: Option<IpAddr>,
        info: InterfaceInfo,
    },
    InterfaceDown {
        name: String,
    },
    AddressAdded {
        interface: String,
        address: IpAddr,
        gateway_ip: Option<IpAddr>,
    },
    AddressDeleted {
        interface: String,
        address: IpAddr,
    },
}

/// Interface info (stub).
#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub name: String,
    pub mac_address: Option<String>,
    pub ip_addresses: Vec<IpAddr>,
    pub is_up: bool,
    pub is_loopback: bool,
}

/// Configuration (stub — all fields accepted but most are ignored).
#[derive(Debug, Clone)]
pub struct NetworkMonitorConfig {
    pub ignore_interfaces: Vec<String>,
    pub only_interfaces: Vec<String>,
    pub throttle_duration: Duration,
    pub event_buffer_size: usize,
}

impl Default for NetworkMonitorConfig {
    fn default() -> Self {
        Self {
            ignore_interfaces: vec!["lo".to_owned()],
            only_interfaces: Vec::new(),
            throttle_duration: Duration::from_secs(2),
            event_buffer_size: 100,
        }
    }
}

/// Error type (stub — always returns connection failure on non-Linux).
#[derive(Debug, thiserror::Error)]
pub enum NetworkMonitorError {
    #[error("netlink connection failed: {0}")]
    ConnectionFailed(String),
    #[error("query failed: {0}")]
    QueryFailed(String),
    #[error("monitor stopped")]
    Stopped,
}

/// Stub network monitor.
///
/// On non-Linux platforms, emits a single fake `InterfaceUp` for `eth0`
/// after a 2-second delay (matching Go v1's non-Linux stub behavior),
/// then sits idle until cancelled.
pub struct NetworkMonitor {
    config: NetworkMonitorConfig,
    cancel: CancellationToken,
}

impl NetworkMonitor {
    /// Create a new stub monitor.
    pub fn new(config: NetworkMonitorConfig) -> Self {
        Self {
            config,
            cancel: CancellationToken::new(),
        }
    }

    /// Get a clone of the cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Start the stub monitor.
    ///
    /// After a 2-second delay, emits a fake `InterfaceUp` for `eth0`
    /// (matching Go v1's `startFakeInterfaceUp` on non-Linux).
    /// Then waits for cancellation.
    pub async fn start(
        &self,
        event_tx: mpsc::Sender<NetworkEvent>,
    ) -> Result<(), NetworkMonitorError> {
        tracing::info!("NetworkMonitor (stub) started — will emit fake eth0 InterfaceUp after 2s");

        // Emit fake event after 2s delay (matches Go v1 stub)
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {
                let name = "eth0".to_owned();
                if self.should_process_interface(&name) {
                    let info = InterfaceInfo {
                        name: name.clone(),
                        mac_address: Some("00:00:00:00:00:00".to_owned()),
                        ip_addresses: vec![],
                        is_up: true,
                        is_loopback: false,
                    };

                    tracing::info!(interface = %name, "Stub: emitting fake InterfaceUp for eth0");

                    let event = NetworkEvent::InterfaceUp {
                        name,
                        gateway_ip: None,
                        info,
                    };
                    if event_tx.send(event).await.is_err() {
                        tracing::warn!("Stub: event channel closed");
                    }
                } else {
                    tracing::debug!("Stub: eth0 filtered out, not emitting fake event");
                }
            }
            () = self.cancel.cancelled() => {
                return Ok(());
            }
        }

        // Wait for cancellation
        self.cancel.cancelled().await;
        Ok(())
    }

    /// Get current interfaces (stub — returns empty list).
    #[allow(clippy::unused_async)]
    pub async fn get_current_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkMonitorError> {
        tracing::debug!("Stub: get_current_interfaces returning empty list");
        Ok(Vec::new())
    }

    /// Get gateway for interface (stub — always returns None).
    #[allow(clippy::unused_async)]
    pub async fn get_gateway_for_interface(
        &self,
        _iface: &str,
    ) -> Result<Option<IpAddr>, NetworkMonitorError> {
        tracing::debug!("Stub: get_gateway_for_interface returning None");
        Ok(None)
    }

    /// Stop the stub monitor.
    #[allow(clippy::unused_async)]
    pub async fn stop(&self) {
        self.cancel.cancel();
    }

    /// Check if interface should be processed (same logic as Linux version).
    fn should_process_interface(&self, name: &str) -> bool {
        if self.config.ignore_interfaces.iter().any(|i| i == name) {
            return false;
        }
        if !self.config.only_interfaces.is_empty()
            && !self.config.only_interfaces.iter().any(|i| i == name)
        {
            return false;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default_values() {
        let config = NetworkMonitorConfig::default();
        assert_eq!(config.ignore_interfaces, vec!["lo"]);
        assert!(config.only_interfaces.is_empty());
        assert_eq!(config.throttle_duration, Duration::from_secs(2));
        assert_eq!(config.event_buffer_size, 100);
    }

    #[test]
    fn test_filter_ignore_interface() {
        let config = NetworkMonitorConfig {
            ignore_interfaces: vec!["lo".to_owned(), "br-lan".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(!monitor.should_process_interface("lo"));
        assert!(!monitor.should_process_interface("br-lan"));
        assert!(monitor.should_process_interface("eth0"));
    }

    #[test]
    fn test_filter_only_interface() {
        let config = NetworkMonitorConfig {
            only_interfaces: vec!["eth0".to_owned(), "wlan0".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(monitor.should_process_interface("eth0"));
        assert!(monitor.should_process_interface("wlan0"));
        assert!(!monitor.should_process_interface("br-lan"));
    }

    #[test]
    fn test_filter_ignore_and_only_combined() {
        let config = NetworkMonitorConfig {
            ignore_interfaces: vec!["eth0".to_owned()],
            only_interfaces: vec!["eth0".to_owned(), "wlan0".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(!monitor.should_process_interface("eth0"));
        assert!(monitor.should_process_interface("wlan0"));
        assert!(!monitor.should_process_interface("usb0"));
    }

    #[tokio::test]
    async fn test_stub_emits_fake_interface_up() {
        let config = NetworkMonitorConfig {
            ignore_interfaces: vec!["lo".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);
        let cancel = monitor.cancel_token();

        let (tx, mut rx) = mpsc::channel(10);

        let handle = tokio::spawn(async move {
            monitor.start(tx).await.unwrap();
        });

        // Wait for the fake event (emitted after 2s, but use 4s to be safe)
        let result = tokio::time::timeout(Duration::from_secs(4), rx.recv()).await;

        cancel.cancel();

        match result {
            Ok(Some(NetworkEvent::InterfaceUp { name, .. })) => {
                assert_eq!(name, "eth0");
            }
            Ok(other) => {
                panic!("Expected InterfaceUp for eth0, got: {other:?}");
            }
            Err(e) => {
                panic!("Timed out waiting for fake InterfaceUp event: {e}");
            }
        }

        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn test_stub_get_current_interfaces_returns_empty() {
        let monitor = NetworkMonitor::new(NetworkMonitorConfig::default());
        let interfaces = monitor.get_current_interfaces().await.unwrap();
        assert!(interfaces.is_empty());
    }

    #[tokio::test]
    async fn test_stub_get_gateway_returns_none() {
        let monitor = NetworkMonitor::new(NetworkMonitorConfig::default());
        let gw = monitor.get_gateway_for_interface("eth0").await.unwrap();
        assert!(gw.is_none());
    }

    #[tokio::test]
    async fn test_stub_stop() {
        let monitor = NetworkMonitor::new(NetworkMonitorConfig::default());
        monitor.stop().await;
        // Should be cancelled
        assert!(monitor.cancel.is_cancelled());
    }
}
