use async_trait::async_trait;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ValveError {
    #[error("valve error: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// ClientStats — mirrors Go's ndsctl json output
// ---------------------------------------------------------------------------

/// Client statistics returned by `ndsctl json <mac>`.
///
/// All byte-count fields from ndsctl are in **kilobytes**; use the
/// `*_bytes()` helpers to convert to bytes.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ClientStats {
    pub id: u64,
    pub ip: String,
    pub mac: String,
    pub added: i64,
    pub active: i64,
    pub duration: i64,
    pub token: String,
    pub state: String,
    /// Downloaded kilobytes (ndsctl unit).
    pub downloaded: u64,
    pub avg_down_speed: f64,
    /// Uploaded kilobytes (ndsctl unit).
    pub uploaded: u64,
    pub avg_up_speed: f64,
}

impl ClientStats {
    /// Convert KB to bytes (ndsctl returns kilobytes).
    pub fn downloaded_bytes(&self) -> u64 {
        self.downloaded * 1024
    }

    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded * 1024
    }

    pub fn total_bytes(&self) -> u64 {
        self.downloaded_bytes() + self.uploaded_bytes()
    }
}

// ---------------------------------------------------------------------------
// MAC validation (Go parity: isValidMAC)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn validate_mac(mac: &str) -> Result<(), ValveError> {
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 {
        return Err(ValveError::Other(format!(
            "invalid MAC address format: {mac}"
        )));
    }
    for part in &parts {
        if part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ValveError::Other(format!(
                "invalid MAC address format: {mac}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Valve trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Valve: Send + Sync {
    async fn open_gate(&self, mac_address: &str) -> Result<(), ValveError>;
    async fn close_gate(&self, mac_address: &str) -> Result<(), ValveError>;

    /// Open gate until a specific unix timestamp. The valve should auto-close at that time.
    /// Default: just calls [`open_gate`](Valve::open_gate) (ignores timestamp).
    async fn open_gate_until(
        &self,
        mac_address: &str,
        until_timestamp: i64,
    ) -> Result<(), ValveError> {
        let _ = until_timestamp;
        self.open_gate(mac_address).await
    }

    /// Get client data stats (download/upload bytes) from ndsctl.
    /// Default: returns zero stats.
    async fn get_client_stats(&self, _mac_address: &str) -> Result<ClientStats, ValveError> {
        Ok(ClientStats::default())
    }

    /// Get total data usage (download + upload) since baseline was set.
    /// Default: returns 0.
    async fn get_client_usage_since_baseline(&self, _mac_address: &str) -> Result<u64, ValveError> {
        Ok(0)
    }

    /// Set the data usage baseline for a MAC (capture current usage as starting point).
    /// Default: no-op.
    async fn set_data_baseline(&self, _mac_address: &str) -> Result<(), ValveError> {
        Ok(())
    }

    /// Clear the data baseline for a MAC.
    /// Default: no-op.
    async fn clear_data_baseline(&self, _mac_address: &str) -> Result<(), ValveError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StubValve — logging-only, no real traffic control
// ---------------------------------------------------------------------------

/// Stub valve that logs gate operations at info level.
///
/// Does not actually gate traffic — real iptables/nftables integration is M4.
/// Use this for development and testing where payment handling is verified
/// without real traffic control.
pub struct StubValve;

#[async_trait]
impl Valve for StubValve {
    async fn open_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::info!(
            mac = mac_address,
            "VALVE OPEN: traffic allowed (stub, no real gating)"
        );
        Ok(())
    }

    async fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::info!(
            mac = mac_address,
            "VALVE CLOSE: session should end, traffic should be blocked (stub, no real gating)"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NoopValve — for RADIUS deployments (enforcement via Session-Timeout + CoA)
// ---------------------------------------------------------------------------

/// A no-op valve for RADIUS deployments where traffic enforcement is handled
/// externally via Session-Timeout and Change-of-Authorization (CoA) messages.
///
/// All operations succeed silently — no iptables/nftables/ndsctl interaction.
pub struct NoopValve;

#[async_trait]
impl Valve for NoopValve {
    async fn open_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::debug!(mac = mac_address, "NOOP: open_gate (RADIUS enforcement)");
        Ok(())
    }

    async fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::debug!(mac = mac_address, "NOOP: close_gate (RADIUS enforcement)");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NdsValve — feature-gated behind "nds"
// ---------------------------------------------------------------------------

#[cfg(feature = "nds")]
mod nds {
    use super::{validate_mac, ClientStats, Valve, ValveError};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio::task::JoinHandle;

    /// Per-MAC data usage baseline (in bytes).
    #[derive(Debug, Clone, Default)]
    struct DataBaseline {
        downloaded_bytes: u64,
        uploaded_bytes: u64,
    }

    /// A gate entry is either indefinite (no timer) or timed (auto-close via tokio task).
    enum GateEntry {
        Indefinite,
        Timed(JoinHandle<()>),
    }

    /// NoDogSplash valve that calls `ndsctl` to authorize/deauthorize clients.
    ///
    /// Mirrors Go v1's `valve.go` exactly:
    /// - `ndsctl_mutex` serializes all ndsctl invocations
    /// - `gates` tracks active MACs with optional auto-close timers
    /// - `data_baselines` tracks per-MAC byte counters for usage metering
    pub struct NdsValve {
        ndsctl_path: PathBuf,
        gates: Arc<Mutex<HashMap<String, GateEntry>>>,
        data_baselines: Arc<Mutex<HashMap<String, DataBaseline>>>,
        ndsctl_mutex: Arc<Mutex<()>>,
    }

    impl NdsValve {
        /// Create a new NdsValve using the default `ndsctl` binary path.
        pub fn new() -> Self {
            Self {
                ndsctl_path: PathBuf::from("ndsctl"),
                gates: Arc::new(Mutex::new(HashMap::new())),
                data_baselines: Arc::new(Mutex::new(HashMap::new())),
                ndsctl_mutex: Arc::new(Mutex::new(())),
            }
        }

        /// Create a new NdsValve with a custom `ndsctl` binary path (for testing).
        pub fn with_ndsctl_path(ndsctl_path: PathBuf) -> Self {
            Self {
                ndsctl_path,
                gates: Arc::new(Mutex::new(HashMap::new())),
                data_baselines: Arc::new(Mutex::new(HashMap::new())),
                ndsctl_mutex: Arc::new(Mutex::new(())),
            }
        }

        // -- async internals (called through block_on from trait impls) ----------

        async fn authorize(&self, mac: &str) -> Result<(), ValveError> {
            let _lock = self.ndsctl_mutex.lock().await;
            let output = tokio::process::Command::new(&self.ndsctl_path)
                .arg("auth")
                .arg(mac)
                .output()
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        ValveError::Other(format!(
                            "ndsctl not found at '{}' — ensure nodogsplash is installed",
                            self.ndsctl_path.display()
                        ))
                    } else {
                        ValveError::Other(format!("ndsctl auth failed: {e}"))
                    }
                })?;
            if !output.status.success() {
                return Err(ValveError::Other(format!(
                    "ndsctl auth failed for {mac}: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            tracing::info!(mac, "ndsctl auth successful");
            Ok(())
        }

        /// Deauthorize a MAC via ndsctl. Mirrors Go: logs error but returns Ok (does not
        /// propagate deauth errors).
        async fn deauthorize(&self, mac: &str) {
            let _lock = self.ndsctl_mutex.lock().await;
            let output = tokio::process::Command::new(&self.ndsctl_path)
                .arg("deauth")
                .arg(mac)
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    tracing::debug!(mac, "ndsctl deauth successful");
                }
                Ok(out) => {
                    tracing::error!(
                        mac,
                        "ndsctl deauth failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                Err(e) => {
                    tracing::error!(mac, "ndsctl deauth failed: {e}");
                }
            }
        }

        async fn fetch_client_stats(&self, mac: &str) -> Result<ClientStats, ValveError> {
            let _lock = self.ndsctl_mutex.lock().await;
            let output = tokio::process::Command::new(&self.ndsctl_path)
                .arg("json")
                .arg(mac)
                .output()
                .await
                .map_err(|e| {
                    ValveError::Other(format!("failed to execute ndsctl json for MAC {mac}: {e}"))
                })?;

            let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if trimmed == "{}" {
                return Err(ValveError::Other(format!(
                    "client with MAC {mac} not found in ndsctl"
                )));
            }

            serde_json::from_str::<ClientStats>(&trimmed).map_err(|e| {
                ValveError::Other(format!("failed to parse ndsctl json for MAC {mac}: {e}"))
            })
        }

        async fn set_data_baseline_inner(&self, mac: &str) -> Result<(), ValveError> {
            match self.fetch_client_stats(mac).await {
                Ok(stats) => {
                    let mut baselines = self.data_baselines.lock().await;
                    baselines.insert(
                        mac.to_owned(),
                        DataBaseline {
                            downloaded_bytes: stats.downloaded_bytes(),
                            uploaded_bytes: stats.uploaded_bytes(),
                        },
                    );
                    tracing::debug!(
                        mac,
                        downloaded = stats.downloaded_bytes(),
                        uploaded = stats.uploaded_bytes(),
                        "data baseline set"
                    );
                    Ok(())
                }
                Err(_) => {
                    // Go: if client not found, use zero baseline
                    let mut baselines = self.data_baselines.lock().await;
                    baselines.insert(
                        mac.to_owned(),
                        DataBaseline {
                            downloaded_bytes: 0,
                            uploaded_bytes: 0,
                        },
                    );
                    tracing::debug!(mac, "data baseline set to zero (client not found)");
                    Ok(())
                }
            }
        }

        async fn clear_data_baseline_inner(&self, mac: &str) {
            let mut baselines = self.data_baselines.lock().await;
            baselines.remove(mac);
        }

        async fn get_client_usage_since_baseline_inner(
            &self,
            mac: &str,
        ) -> Result<u64, ValveError> {
            let stats = self.fetch_client_stats(mac).await?;
            let baselines = self.data_baselines.lock().await;
            let baseline = baselines.get(mac).cloned().unwrap_or_default();
            let dl = stats
                .downloaded_bytes()
                .saturating_sub(baseline.downloaded_bytes);
            let ul = stats
                .uploaded_bytes()
                .saturating_sub(baseline.uploaded_bytes);
            Ok(dl + ul)
        }

        async fn open_gate_inner(&self, mac: &str) -> Result<(), ValveError> {
            let mut gates = self.gates.lock().await;

            // Stop existing timer if any (mirrors Go: if existingTimer != nil { existingTimer.Stop() })
            if let Some(GateEntry::Timed(handle)) = gates.remove(mac) {
                handle.abort();
            }

            self.authorize(mac).await?;

            if let Err(e) = self.set_data_baseline_inner(mac).await {
                tracing::warn!(mac, "Failed to set data baseline, continuing anyway: {e}");
            }

            gates.insert(mac.to_owned(), GateEntry::Indefinite);
            Ok(())
        }

        async fn open_gate_until_inner(
            &self,
            mac: &str,
            until_timestamp: i64,
        ) -> Result<(), ValveError> {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let duration_secs = until_timestamp - now;
            if duration_secs <= 0 {
                return Err(ValveError::Other(format!(
                    "timestamp {until_timestamp} is in the past (current time: {now})"
                )));
            }

            tracing::info!(
                mac,
                until_timestamp,
                duration_secs,
                "Opening gate until timestamp"
            );

            let mut gates = self.gates.lock().await;

            // Always stop existing timer + re-auth (Go parity: openGateForSession
            // always calls ndsctl auth regardless of existing gate state).
            let existing = gates.remove(mac);
            if let Some(GateEntry::Timed(handle)) = existing {
                handle.abort();
            }

            self.authorize(mac).await?;

            if let Err(e) = self.set_data_baseline_inner(mac).await {
                tracing::warn!(mac, "Failed to set data baseline, continuing anyway: {e}");
            }

            let gates_clone = self.gates.clone();
            let ndsctl_path = self.ndsctl_path.clone();
            let ndsctl_mutex = self.ndsctl_mutex.clone();
            let data_baselines = self.data_baselines.clone();
            let mac_owned = mac.to_owned();

            let handle = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(duration_secs as u64)).await;

                // deauthorize inline (we own the pieces)
                {
                    let _lock = ndsctl_mutex.lock().await;
                    let output = tokio::process::Command::new(&ndsctl_path)
                        .arg("deauth")
                        .arg(&mac_owned)
                        .output()
                        .await;
                    match output {
                        Ok(out) if out.status.success() => {
                            tracing::debug!(
                                mac = mac_owned,
                                "ndsctl deauth successful (auto-close)"
                            );
                        }
                        Ok(out) => {
                            tracing::error!(
                                mac = mac_owned,
                                "Error deauthorizing MAC after timeout: {}",
                                String::from_utf8_lossy(&out.stderr)
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                mac = mac_owned,
                                "Error deauthorizing MAC after timeout: {e}"
                            );
                        }
                    }
                }

                {
                    let mut g = gates_clone.lock().await;
                    g.remove(&mac_owned);
                }
                {
                    let mut b = data_baselines.lock().await;
                    b.remove(&mac_owned);
                }
            });

            gates.insert(mac.to_owned(), GateEntry::Timed(handle));
            Ok(())
        }

        async fn close_gate_inner(&self, mac: &str) -> Result<(), ValveError> {
            let mut gates = self.gates.lock().await;

            // Go: even if not in gates, still attempt deauth (state may be out of sync)

            self.deauthorize(mac).await;

            if let Some(GateEntry::Timed(handle)) = gates.remove(mac) {
                handle.abort();
            } else {
                gates.remove(mac);
            }

            self.clear_data_baseline_inner(mac).await;

            Ok(())
        }
    }

    #[async_trait]
    impl Valve for NdsValve {
        async fn open_gate(&self, mac_address: &str) -> Result<(), ValveError> {
            validate_mac(mac_address)?;
            self.open_gate_inner(mac_address).await
        }

        async fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
            validate_mac(mac_address)?;
            self.close_gate_inner(mac_address).await
        }

        async fn open_gate_until(
            &self,
            mac_address: &str,
            until_timestamp: i64,
        ) -> Result<(), ValveError> {
            validate_mac(mac_address)?;
            self.open_gate_until_inner(mac_address, until_timestamp)
                .await
        }

        async fn get_client_stats(&self, mac_address: &str) -> Result<ClientStats, ValveError> {
            validate_mac(mac_address)?;
            self.fetch_client_stats(mac_address).await
        }

        async fn get_client_usage_since_baseline(
            &self,
            mac_address: &str,
        ) -> Result<u64, ValveError> {
            validate_mac(mac_address)?;
            self.get_client_usage_since_baseline_inner(mac_address)
                .await
        }

        async fn set_data_baseline(&self, mac_address: &str) -> Result<(), ValveError> {
            validate_mac(mac_address)?;
            self.set_data_baseline_inner(mac_address).await
        }

        async fn clear_data_baseline(&self, mac_address: &str) -> Result<(), ValveError> {
            validate_mac(mac_address)?;
            self.clear_data_baseline_inner(mac_address).await;
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Tests (feature-gated, require tokio runtime)
    // -----------------------------------------------------------------------

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;

        /// Create a tempdir with a mock-ndsctl shell script.
        fn create_mock_ndsctl() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let script_path = dir.path().join("mock-ndsctl");
            let mut f = std::fs::File::create(&script_path).expect("create script");
            write!(
                f,
                "#!/bin/sh\n\
                 if [ \"$1\" = \"auth\" ]; then\n\
                 echo \"Client $2 authenticated.\"\n\
                 exit 0\n\
                 elif [ \"$1\" = \"deauth\" ]; then\n\
                 echo \"Client $2 deauthenticated.\"\n\
                 exit 0\n\
                 elif [ \"$1\" = \"json\" ]; then\n\
                 echo '{{\"id\":1,\"ip\":\"192.168.1.100\",\"mac\":\"'$2'\",\"added\":1000,\"active\":2000,\"duration\":1000,\"token\":\"abc\",\"state\":\"AUTHORIZED\",\"downloaded\":1024,\"avg_down_speed\":0,\"uploaded\":512,\"avg_up_speed\":0}}'\n\
                 exit 0\n\
                 fi\n\
                 exit 1\n"
            )
            .expect("write script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            (dir, script_path)
        }

        /// Create a mock that returns `{}` for json queries (client not found).
        fn create_mock_ndsctl_not_found() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let script_path = dir.path().join("mock-ndsctl");
            let mut f = std::fs::File::create(&script_path).expect("create script");
            write!(
                f,
                "#!/bin/sh\n\
                 if [ \"$1\" = \"auth\" ]; then\n\
                 echo \"Client $2 authenticated.\"\n\
                 exit 0\n\
                 elif [ \"$1\" = \"deauth\" ]; then\n\
                 echo \"Client $2 deauthenticated.\"\n\
                 exit 0\n\
                 elif [ \"$1\" = \"json\" ]; then\n\
                 echo '{{}}'\n\
                 exit 0\n\
                 fi\n\
                 exit 1\n"
            )
            .expect("write script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            (dir, script_path)
        }

        #[tokio::test]
        async fn test_nds_auth_success() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);
            let result = valve.authorize("aa:bb:cc:dd:ee:ff").await;
            assert!(result.is_ok(), "authorize should succeed");
        }

        #[tokio::test]
        async fn test_nds_deauth_success() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);
            valve.deauthorize("aa:bb:cc:dd:ee:ff").await;
        }

        #[tokio::test]
        async fn test_nds_open_gate_indefinite() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);
            let result = valve.open_gate_inner("aa:bb:cc:dd:ee:ff").await;
            assert!(result.is_ok(), "open_gate should succeed");

            let gates = valve.gates.lock().await;
            assert!(
                gates.contains_key("aa:bb:cc:dd:ee:ff"),
                "MAC should be in gates map"
            );
            assert!(
                matches!(gates.get("aa:bb:cc:dd:ee:ff"), Some(GateEntry::Indefinite)),
                "gate should be Indefinite"
            );
        }

        #[tokio::test]
        async fn test_nds_open_gate_until() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            let future_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600;

            let result = valve
                .open_gate_until_inner("aa:bb:cc:dd:ee:ff", future_ts)
                .await;
            assert!(result.is_ok(), "open_gate_until should succeed");

            let gates = valve.gates.lock().await;
            assert!(
                gates.contains_key("aa:bb:cc:dd:ee:ff"),
                "MAC should be in gates map"
            );
            assert!(
                matches!(gates.get("aa:bb:cc:dd:ee:ff"), Some(GateEntry::Timed(_))),
                "gate should be Timed"
            );
        }

        #[tokio::test]
        async fn test_nds_open_gate_until_past_timestamp() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            let past_ts = 1;
            let result = valve
                .open_gate_until_inner("aa:bb:cc:dd:ee:ff", past_ts)
                .await;
            assert!(result.is_err(), "past timestamp should return error");
        }

        #[tokio::test]
        async fn test_nds_close_gate() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            valve.open_gate_inner("aa:bb:cc:dd:ee:ff").await.unwrap();
            let result = valve.close_gate_inner("aa:bb:cc:dd:ee:ff").await;
            assert!(result.is_ok(), "close_gate should succeed");

            let gates = valve.gates.lock().await;
            assert!(
                !gates.contains_key("aa:bb:cc:dd:ee:ff"),
                "MAC should be removed from gates map"
            );
        }

        #[tokio::test]
        async fn test_nds_close_unopened_gate() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            // Go: deauth attempted even for unknown MACs (state may be out of sync)
            let result = valve.close_gate_inner("11:22:33:44:55:66").await;
            assert!(result.is_ok(), "close_gate on unopened gate should succeed");
        }

        #[tokio::test]
        async fn test_nds_reopen_extends_timer() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            let future_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600;

            valve
                .open_gate_until_inner("aa:bb:cc:dd:ee:ff", future_ts)
                .await
                .unwrap();

            // Reopen with new timestamp replaces existing timer (Go: existingTimer.Stop())
            let future_ts2 = future_ts + 3600;
            valve
                .open_gate_until_inner("aa:bb:cc:dd:ee:ff", future_ts2)
                .await
                .unwrap();

            let gates = valve.gates.lock().await;
            assert!(gates.contains_key("aa:bb:cc:dd:ee:ff"));
            assert!(
                matches!(gates.get("aa:bb:cc:dd:ee:ff"), Some(GateEntry::Timed(_))),
                "gate should still be Timed after reopen"
            );
        }

        #[tokio::test]
        async fn test_nds_get_client_stats() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            let stats = valve.fetch_client_stats("aa:bb:cc:dd:ee:ff").await.unwrap();
            assert_eq!(stats.id, 1);
            assert_eq!(stats.ip, "192.168.1.100");
            assert_eq!(stats.mac, "aa:bb:cc:dd:ee:ff");
            assert_eq!(stats.state, "AUTHORIZED");
            assert_eq!(stats.downloaded, 1024);
            assert_eq!(stats.uploaded, 512);
            assert_eq!(stats.downloaded_bytes(), 1024 * 1024);
            assert_eq!(stats.uploaded_bytes(), 512 * 1024);
        }

        #[tokio::test]
        async fn test_nds_get_client_stats_not_found() {
            let (_dir, path) = create_mock_ndsctl_not_found();
            let valve = NdsValve::with_ndsctl_path(path);

            let result = valve.fetch_client_stats("aa:bb:cc:dd:ee:ff").await;
            assert!(result.is_err(), "empty {{}} response should return error");
        }

        #[tokio::test]
        async fn test_nds_data_baseline() {
            let (_dir, path) = create_mock_ndsctl();
            let valve = NdsValve::with_ndsctl_path(path);

            valve.open_gate_inner("aa:bb:cc:dd:ee:ff").await.unwrap();

            let baselines = valve.data_baselines.lock().await;
            let baseline = baselines
                .get("aa:bb:cc:dd:ee:ff")
                .expect("baseline should exist");
            assert_eq!(baseline.downloaded_bytes, 1024 * 1024);
            assert_eq!(baseline.uploaded_bytes, 512 * 1024);
        }

        #[tokio::test]
        async fn test_nds_data_baseline_zero_when_not_found() {
            let (_dir, path) = create_mock_ndsctl_not_found();
            let valve = NdsValve::with_ndsctl_path(path);

            // Go: zero baseline when client not found
            valve
                .set_data_baseline_inner("aa:bb:cc:dd:ee:ff")
                .await
                .unwrap();

            let baselines = valve.data_baselines.lock().await;
            let baseline = baselines
                .get("aa:bb:cc:dd:ee:ff")
                .expect("baseline should exist");
            assert_eq!(baseline.downloaded_bytes, 0);
            assert_eq!(baseline.uploaded_bytes, 0);
        }
    }
}

#[cfg(feature = "nds")]
pub use nds::NdsValve;

// ---------------------------------------------------------------------------
// Tests for StubValve (always compiled)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stub_valve_still_works() {
        let valve = StubValve;
        assert!(valve.open_gate("aa:bb:cc:dd:ee:ff").await.is_ok());
        assert!(valve.close_gate("aa:bb:cc:dd:ee:ff").await.is_ok());
    }

    #[tokio::test]
    async fn test_stub_valve_defaults() {
        let valve = StubValve;
        assert!(valve
            .open_gate_until("aa:bb:cc:dd:ee:ff", 9_999_999_999)
            .await
            .is_ok());

        let stats = valve.get_client_stats("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(stats.downloaded, 0);
        assert_eq!(stats.uploaded, 0);

        assert_eq!(
            valve
                .get_client_usage_since_baseline("aa:bb:cc:dd:ee:ff")
                .await
                .unwrap(),
            0
        );

        assert!(valve.set_data_baseline("aa:bb:cc:dd:ee:ff").await.is_ok());
        assert!(valve.clear_data_baseline("aa:bb:cc:dd:ee:ff").await.is_ok());
    }
}
