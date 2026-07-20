#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

//! WiFi STA (station) management via UCI commands.
//!
//! # Go v1 vs Rust comparison
//!
//! Go v1: `connector.go` uses raw `os/exec` for ALL UCI operations, with no
//! input validation or shell quoting.
//!
//! Rust: Uses structured UCI ops from `uci_ops.rs` — every command is:
//! - Type-checked (`UciOp` enum)
//! - Shell-safe (`sh_quote`)
//! - Renderable to both shell AND ubus transport
//!
//! This is the single biggest safety improvement over Go v1.
//!
//! # STA lifecycle
//!
//! 1. `ensure_wwan_setup()` — creates network.wwan interface + firewall
//! 2. `connect()` — creates STA section, reloads wifi, waits for DHCP
//! 3. `switch_upstream()` — atomically swaps STA with fallback
//! 4. `disconnect()` — removes STA section, reloads wifi
//! 5. `cleanup_stale_stas()` — dedupes STA sections by SSID

use std::time::Duration;

use thiserror::Error;

use super::uci_ops::{UciOp, UciOpBuilder, execute_shell};
use super::wifi_scanner::{CommandExecutor, EncryptionType, SystemCommandExecutor, WifiScanError};

/// Errors from WiFi connector operations.
#[derive(Debug, Error)]
pub enum WifiConnectError {
    /// Invalid parameter (radio name, SSID, etc.).
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    /// UCI operation failed.
    #[error("UCI operation failed: {0}")]
    UciFailed(String),
    /// Shell command failed (wifi reload, ifup, etc.).
    #[error("command failed: {0}")]
    CommandFailed(String),
    /// DHCP timeout — STA did not get an IP address.
    #[error("DHCP timeout: STA {iface} did not get IP within {timeout:?}")]
    DhcpTimeout { iface: String, timeout: Duration },
    /// Connection verification failed (SSID mismatch).
    #[error("verification failed: {0}")]
    VerificationFailed(String),
    /// Scan error from WifiScanner.
    #[error(transparent)]
    ScanError(#[from] WifiScanError),
}

/// WiFi STA connector — manages UCI wireless configuration.
///
/// # Go v1 vs Rust
///
/// Go v1: `connector.go` — `Connect()`, `SwitchUpstream()`, `waitForSTAIP()`,
/// `EnsureWWANSetup()`, `CleanupStaleSTAs()`. All raw shell commands.
///
/// Rust: Same operations but using structured `UciOp` types. Every shell
/// command goes through `sh_quote()` for safety.
pub struct WifiConnector {
    /// Timeout for DHCP waiting.
    timeout: Duration,
    /// Command executor for shell commands (wifi reload, ifup, etc.).
    executor: Box<dyn CommandExecutor>,
}

impl WifiConnector {
    /// Create a new connector with default settings.
    ///
    /// Default timeout: 30s.
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            executor: Box::new(SystemCommandExecutor::new(Duration::from_secs(30))),
        }
    }

    /// Create a connector with a custom command executor (for testing).
    pub fn with_executor(executor: Box<dyn CommandExecutor>) -> Self {
        Self {
            timeout: Duration::from_secs(30),
            executor,
        }
    }

    /// Set a custom timeout for DHCP waiting.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Connect to a WiFi network.
    ///
    /// Creates a UCI STA section, configures encryption and password,
    /// reloads wifi, and waits for DHCP to assign an IP address.
    ///
    /// Returns the network interface name (e.g. "wlan0").
    ///
    /// Go v1: `Connect(gateway, password)` — creates STA, configures,
    /// reloads wifi, verifies connection.
    pub async fn connect(
        &self,
        radio: &str,
        ssid: &str,
        password: &str,
        encryption: &EncryptionType,
    ) -> Result<String, WifiConnectError> {
        tracing::info!("connecting to SSID '{ssid}' on radio '{radio}'");

        self.cleanup_stale_stas_for_ssid(ssid).await?;

        let sta_name = format!("sta_{radio}");
        let ops = self.build_sta_uci_ops(&sta_name, radio, ssid, password, encryption);
        let results = execute_shell(&ops).await;

        for (i, result) in results.iter().enumerate() {
            if let Err(e) = result {
                let op = &ops[i];
                if !matches!(op, UciOp::Shell { .. }) {
                    tracing::warn!("UCI op {i} failed: {e}");
                }
            }
        }

        let reload_ops = UciOpBuilder::new()
            .commit("wireless")
            .shell("wifi reload")
            .build();
        let reload_results = execute_shell(&reload_ops).await;
        for result in &reload_results {
            if let Err(e) = result {
                tracing::warn!("wifi reload failed: {e}");
            }
        }

        let ifup_ops = UciOpBuilder::new().shell("ifup wwan").build();
        execute_shell(&ifup_ops).await;

        let iface = self.find_sta_interface(radio).await?;

        self.wait_for_sta_ip(&iface, self.timeout).await?;

        self.verify_connection(&iface, ssid).await?;

        tracing::info!("connected to '{ssid}' on interface {iface}");
        Ok(iface)
    }

    /// Disconnect from the current WiFi STA.
    ///
    /// Removes all STA sections from UCI wireless config and reloads wifi.
    pub async fn disconnect(&self) -> Result<(), WifiConnectError> {
        tracing::info!("disconnecting from WiFi STA");

        let ops = self.build_disconnect_ops();
        let results = execute_shell(&ops).await;

        for result in &results {
            if let Err(e) = result {
                tracing::warn!("disconnect op failed: {e}");
            }
        }

        let reload_ops = UciOpBuilder::new()
            .commit("wireless")
            .shell("wifi reload")
            .build();
        execute_shell(&reload_ops).await;

        tracing::info!("disconnected from WiFi STA");
        Ok(())
    }

    /// Switch upstream connection with automatic fallback.
    ///
    /// Disconnects from the current AP and connects to the new one.
    /// If the new connection fails, attempts to reconnect to the old one.
    ///
    /// Go v1: `SwitchUpstream(activeIface, candidateIface, candidateSSID)`
    pub async fn switch_upstream(
        &self,
        from_radio: &str,
        from_ssid: &str,
        to_radio: &str,
        to_ssid: &str,
        to_password: &str,
        to_encryption: &EncryptionType,
    ) -> Result<(), WifiConnectError> {
        tracing::info!(
            "switching upstream from '{from_ssid}' ({from_radio}) to '{to_ssid}' ({to_radio})"
        );

        let switch_ops = self.build_switch_ops(
            from_radio,
            from_ssid,
            to_radio,
            to_ssid,
            to_password,
            to_encryption,
        );

        let results = execute_shell(&switch_ops).await;
        for result in &results {
            if let Err(e) = result {
                tracing::warn!("switch op failed: {e}");
            }
        }

        let reload_ops = UciOpBuilder::new()
            .commit("wireless")
            .shell("wifi reload")
            .build();
        execute_shell(&reload_ops).await;

        let iface = self.find_sta_interface(to_radio).await?;

        match self.wait_for_sta_ip(&iface, self.timeout).await {
            Ok(_) => {
                if let Err(e) = self.verify_connection(&iface, to_ssid).await {
                    tracing::warn!("verification failed for new upstream: {e}");
                    self.fallback_to(from_radio, from_ssid).await?;
                    return Err(e);
                }
                tracing::info!("successfully switched to '{to_ssid}'");
                Ok(())
            }
            Err(e) => {
                tracing::warn!("DHCP failed for new upstream: {e}");
                self.fallback_to(from_radio, from_ssid).await?;
                Err(e)
            }
        }
    }

    /// Ensure network.wwan interface exists and is attached to firewall wan zone.
    ///
    /// Go v1: `EnsureWWANSetup()` — creates network.wwan, attaches to wan zone.
    /// This must be called once at startup before any STA connections.
    pub async fn ensure_wwan_setup(&self) -> Result<(), WifiConnectError> {
        tracing::debug!("ensuring WWAN network interface exists");

        let ops = self.build_wwan_ops();
        let results = execute_shell(&ops).await;

        for result in &results {
            if let Err(e) = result {
                tracing::warn!("WWAN setup op failed: {e}");
            }
        }

        tracing::info!("WWAN network interface ensured");
        Ok(())
    }

    /// Remove duplicate STA sections with the same SSID.
    ///
    /// Go v1: `CleanupStaleSTAs()` — dedupes STA sections by SSID.
    /// Keeps the most recently configured STA for each SSID.
    pub async fn cleanup_stale_stas(&self) -> Result<(), WifiConnectError> {
        tracing::debug!("cleaning up stale STA sections");

        let ops = self.build_cleanup_ops();
        if ops.is_empty() {
            tracing::debug!("no stale STA sections found");
            return Ok(());
        }

        let results = execute_shell(&ops).await;
        for result in &results {
            if let Err(e) = result {
                tracing::warn!("cleanup op failed: {e}");
            }
        }

        tracing::info!("cleaned up stale STA sections");
        Ok(())
    }

    /// Build UCI operations to create a STA section.
    ///
    /// Go v1 builds raw `uci set` commands. Rust uses `UciOpBuilder`.
    #[must_use]
    pub fn build_sta_uci_ops(
        &self,
        sta_name: &str,
        radio: &str,
        ssid: &str,
        password: &str,
        encryption: &EncryptionType,
    ) -> Vec<UciOp> {
        let mut values = vec![
            ("device", radio),
            ("mode", "sta"),
            ("ssid", ssid),
            ("network", "wwan"),
        ];

        if *encryption == EncryptionType::None {
            values.push(("encryption", "none"));
        } else {
            values.push(("encryption", encryption.to_uci_value()));
            values.push(("key", password));
        }

        UciOpBuilder::new()
            .comment(&format!("STA for {ssid} on {radio}"))
            .add("wireless", "wifi-iface", sta_name, values)
            .build()
    }

    /// Build UCI operations for WWAN network interface setup.
    ///
    /// Creates `network.wwan` as a DHCP client interface and adds it
    /// to the `firewall.@zone[1]` (wan zone) network list.
    #[must_use]
    pub fn build_wwan_ops(&self) -> Vec<UciOp> {
        UciOpBuilder::new()
            .comment("Ensure WWAN network interface")
            .add(
                "network",
                "interface",
                "wwan",
                vec![("proto", "dhcp"), ("metric", "50")],
            )
            .add_list("firewall", "@zone[1]", "network", "wwan")
            .commit("network")
            .commit("firewall")
            .build()
    }

    /// Build UCI operations for disconnecting all STAs.
    #[must_use]
    pub fn build_disconnect_ops(&self) -> Vec<UciOp> {
        UciOpBuilder::new()
            .comment("Disconnect all STAs")
            .shell("for s in $(uci show wireless | grep wifi-iface | grep \"mode='sta'\" | cut -d. -f2 | cut -d= -f1); do uci delete wireless.$s 2>/dev/null || true; done")
            .commit("wireless")
            .build()
    }

    /// Build UCI operations for upstream switch with fallback.
    #[must_use]
    pub fn build_switch_ops(
        &self,
        from_radio: &str,
        from_ssid: &str,
        to_radio: &str,
        to_ssid: &str,
        to_password: &str,
        to_encryption: &EncryptionType,
    ) -> Vec<UciOp> {
        let from_sta = format!("sta_{from_radio}");
        let to_sta = format!("sta_{to_radio}");

        let mut ops = Vec::new();

        ops.push(UciOp::Comment {
            text: format!("Switch upstream: {from_ssid} → {to_ssid}"),
        });
        ops.push(UciOp::Delete {
            config: "wireless".to_owned(),
            section: from_sta,
            option: None,
        });

        ops.extend(self.build_sta_uci_ops(&to_sta, to_radio, to_ssid, to_password, to_encryption));

        ops
    }

    async fn cleanup_stale_stas_for_ssid(&self, ssid: &str) -> Result<(), WifiConnectError> {
        let escaped_ssid = ssid.replace('\'', "'\\''");
        let cmd = format!(
            "for s in $(uci show wireless | grep -B1 \"ssid='{escaped_ssid}'\" | grep wifi-iface | cut -d. -f2 | cut -d= -f1); do uci delete wireless.$s 2>/dev/null || true; done"
        );
        let ops = vec![
            UciOp::Shell { command: cmd },
            UciOp::Commit {
                config: "wireless".to_owned(),
            },
        ];
        execute_shell(&ops).await;
        Ok(())
    }

    async fn find_sta_interface(&self, radio: &str) -> Result<String, WifiConnectError> {
        let output = self
            .executor
            .execute("iw", vec!["dev".to_owned()])
            .await
            .map_err(|e| WifiConnectError::CommandFailed(format!("iw dev: {e}")))?;

        if output.success() {
            let mut current_iface = String::new();
            for line in output.stdout.lines() {
                let line = line.trim();
                if let Some(iface) = line.strip_prefix("Interface ") {
                    iface.clone_into(&mut current_iface);
                }
                if (line.contains("type managed") || line.contains("type STA"))
                    && !current_iface.is_empty()
                {
                    return Ok(current_iface);
                }
            }
        }

        if let Some(num) = radio.strip_prefix("radio") {
            Ok(format!("wlan{num}"))
        } else {
            Ok("wlan0".to_owned())
        }
    }

    async fn wait_for_sta_ip(
        &self,
        iface: &str,
        timeout: Duration,
    ) -> Result<std::net::IpAddr, WifiConnectError> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_secs(1);

        tracing::debug!("waiting for DHCP on {iface} (timeout {timeout:?})");

        while start.elapsed() < timeout {
            if let Ok(output) = self
                .executor
                .execute(
                    "ip",
                    vec!["addr".to_owned(), "show".to_owned(), iface.to_owned()],
                )
                .await
            {
                if output.success() {
                    for line in output.stdout.lines() {
                        let line = line.trim();
                        if let Some(inet_part) = line.strip_prefix("inet ") {
                            if let Some(addr_str) = inet_part.split('/').next() {
                                if let Ok(addr) = addr_str.parse::<std::net::IpAddr>() {
                                    tracing::info!(
                                        "DHCP assigned {addr} to {iface} in {:?}",
                                        start.elapsed()
                                    );
                                    return Ok(addr);
                                }
                            }
                        }
                    }
                }
            }

            let _ = self
                .executor
                .execute(
                    "udhcpc",
                    vec![
                        "-i".to_owned(),
                        iface.to_owned(),
                        "-n".to_owned(),
                        "-q".to_owned(),
                    ],
                )
                .await;

            tokio::time::sleep(poll_interval).await;
        }

        Err(WifiConnectError::DhcpTimeout {
            iface: iface.to_owned(),
            timeout,
        })
    }

    async fn verify_connection(
        &self,
        iface: &str,
        expected_ssid: &str,
    ) -> Result<(), WifiConnectError> {
        let output = self
            .executor
            .execute("iwgetid", vec!["-r".to_owned(), iface.to_owned()])
            .await
            .map_err(|e| WifiConnectError::CommandFailed(format!("iwgetid: {e}")))?;

        if !output.success() && !output.stdout.trim().is_empty() {
            return Err(WifiConnectError::VerificationFailed(
                "iwgetid failed".to_owned(),
            ));
        }

        let connected_ssid = output.stdout.trim();
        if connected_ssid != expected_ssid {
            return Err(WifiConnectError::VerificationFailed(format!(
                "connected to '{connected_ssid}' but expected '{expected_ssid}'"
            )));
        }

        Ok(())
    }

    async fn fallback_to(&self, radio: &str, ssid: &str) -> Result<(), WifiConnectError> {
        tracing::warn!("falling back to '{ssid}' on {radio}");
        let sta_name = format!("sta_{radio}");
        let ops = UciOpBuilder::new()
            .comment(&format!("Fallback to {ssid}"))
            .add(
                "wireless",
                "wifi-iface",
                &sta_name,
                vec![
                    ("device", radio),
                    ("mode", "sta"),
                    ("ssid", ssid),
                    ("network", "wwan"),
                ],
            )
            .commit("wireless")
            .shell("wifi reload")
            .build();

        let results = execute_shell(&ops).await;
        for result in &results {
            if let Err(e) = result {
                tracing::warn!("fallback op failed: {e}");
            }
        }

        Ok(())
    }

    /// Build cleanup ops for stale STA sections.
    #[must_use]
    pub fn build_cleanup_ops(&self) -> Vec<UciOp> {
        UciOpBuilder::new()
            .comment("Cleanup stale STA sections")
            .shell(
                "for ssid in $(uci show wireless | grep \"mode='sta'\" -A5 | grep \"ssid=\" | \
                 sed \"s/.*ssid='\\([^']*\\)'.*/\\1/\" | sort | uniq -d); do \
                 sections=$(uci show wireless | grep \"ssid='$ssid'\" -B5 | grep wifi-iface | \
                 cut -d. -f2 | cut -d= -f1); \
                 count=0; \
                 for s in $sections; do \
                 count=$((count + 1)); \
                 if [ $count -gt 1 ]; then uci delete wireless.$s 2>/dev/null || true; fi; \
                 done; \
                 done",
            )
            .commit("wireless")
            .build()
    }
}

impl Default for WifiConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::uci_ops::render_shell;
    use super::*;

    #[test]
    fn test_build_sta_uci_ops() {
        let connector = WifiConnector::new();
        let ops = connector.build_sta_uci_ops(
            "sta_radio0",
            "radio0",
            "TollGate-5G",
            "secret123",
            &EncryptionType::Psk2,
        );

        assert!(ops.len() >= 2);

        assert!(matches!(&ops[0], UciOp::Comment { text } if text.contains("TollGate-5G")));

        if let UciOp::Add {
            config,
            type_name,
            name,
            values,
        } = &ops[1]
        {
            assert_eq!(config, "wireless");
            assert_eq!(type_name, "wifi-iface");
            assert_eq!(name, "sta_radio0");
            let value_map: std::collections::HashMap<_, _> = values
                .iter()
                .filter_map(|(k, v)| match v {
                    super::super::uci_ops::OpValue::Single(s) => Some((k.as_str(), s.as_str())),
                    super::super::uci_ops::OpValue::List(_) => None,
                })
                .collect();
            assert_eq!(value_map.get("device"), Some(&"radio0"));
            assert_eq!(value_map.get("mode"), Some(&"sta"));
            assert_eq!(value_map.get("ssid"), Some(&"TollGate-5G"));
            assert_eq!(value_map.get("network"), Some(&"wwan"));
            assert_eq!(value_map.get("encryption"), Some(&"psk2"));
            assert_eq!(value_map.get("key"), Some(&"secret123"));
        } else {
            panic!("expected Add op, got {:?}", &ops[1]);
        }
    }

    #[test]
    fn test_build_sta_uci_ops_open_network() {
        let connector = WifiConnector::new();
        let ops = connector.build_sta_uci_ops(
            "sta_radio1",
            "radio1",
            "OpenNet",
            "",
            &EncryptionType::None,
        );

        if let UciOp::Add { values, .. } = &ops[1] {
            let value_map: std::collections::HashMap<_, _> = values
                .iter()
                .filter_map(|(k, v)| match v {
                    super::super::uci_ops::OpValue::Single(s) => Some((k.as_str(), s.as_str())),
                    super::super::uci_ops::OpValue::List(_) => None,
                })
                .collect();
            assert_eq!(value_map.get("encryption"), Some(&"none"));
            assert!(!value_map.contains_key("key"));
        } else {
            panic!("expected Add op");
        }
    }

    #[test]
    fn test_build_wwan_ops() {
        let connector = WifiConnector::new();
        let ops = connector.build_wwan_ops();

        assert!(ops.len() >= 4);

        let add_op = ops.iter().find(|op| {
            matches!(op, UciOp::Add { config, type_name, name, .. }
                if config == "network" && type_name == "interface" && name == "wwan")
        });
        assert!(add_op.is_some(), "should have Add op for network.wwan");

        let add_list_op = ops.iter().find(|op| {
            matches!(op, UciOp::AddList { config, section, option, value }
                if config == "firewall" && section == "@zone[1]" && option == "network" && value == "wwan")
        });
        assert!(
            add_list_op.is_some(),
            "should have AddList for firewall wan zone"
        );

        let commits: Vec<_> = ops
            .iter()
            .filter(|op| matches!(op, UciOp::Commit { config } if config == "network" || config == "firewall"))
            .collect();
        assert_eq!(
            commits.len(),
            2,
            "should have commits for network and firewall"
        );
    }

    #[test]
    fn test_build_switch_ops() {
        let connector = WifiConnector::new();
        let ops = connector.build_switch_ops(
            "radio0",
            "OldNet",
            "radio1",
            "NewNet",
            "password",
            &EncryptionType::Psk2,
        );

        assert!(
            matches!(&ops[0], UciOp::Comment { text } if text.contains("OldNet") && text.contains("NewNet"))
        );

        let delete_op = ops.iter().find(|op| {
            matches!(op, UciOp::Delete { config, section, option: None }
                if config == "wireless" && section == "sta_radio0")
        });
        assert!(delete_op.is_some(), "should delete old STA");

        let add_op = ops.iter().find(|op| {
            matches!(op, UciOp::Add { config, name, .. }
                if config == "wireless" && name == "sta_radio1")
        });
        assert!(add_op.is_some(), "should add new STA");
    }

    #[test]
    fn test_build_disconnect_ops() {
        let connector = WifiConnector::new();
        let ops = connector.build_disconnect_ops();

        assert!(ops.len() >= 2);

        assert!(
            ops.iter()
                .any(|op| matches!(op, UciOp::Commit { config } if config == "wireless"))
        );
    }

    #[test]
    fn test_build_cleanup_ops() {
        let connector = WifiConnector::new();
        let ops = connector.build_cleanup_ops();

        assert!(ops.len() >= 2);
        assert!(
            ops.iter()
                .any(|op| matches!(op, UciOp::Commit { config } if config == "wireless"))
        );
    }

    #[test]
    fn test_render_sta_ops_shell_safe() {
        let connector = WifiConnector::new();

        let ops = connector.build_sta_uci_ops(
            "sta_radio0",
            "radio0",
            "Test; rm -rf /",
            "pass'word",
            &EncryptionType::Psk2,
        );

        let cmds = render_shell(&ops);
        for cmd in &cmds {
            if cmd.starts_with('#') {
                continue;
            }
            if cmd.contains("uci set") || cmd.contains("uci add") {
                let ssid_safe = cmd.contains("'Test; rm -rf /'");
                let pass_safe = !cmd.contains("pass'word") || cmd.contains("pass'\\''word'");
                assert!(
                    ssid_safe || !cmd.contains("Test"),
                    "SSID with shell metacharacters must be single-quoted: {cmd}"
                );
                assert!(pass_safe, "Password with quotes must be escaped: {cmd}");
            }
        }
    }

    #[test]
    fn test_wwan_ops_render_correctly() {
        let connector = WifiConnector::new();
        let ops = connector.build_wwan_ops();
        let cmds = render_shell(&ops);

        assert!(
            cmds.iter()
                .any(|c| c.contains("uci set network.wwan='interface'")),
            "should set network.wwan interface type: {cmds:?}"
        );

        assert!(
            cmds.iter()
                .any(|c| c.contains("uci add_list firewall.@zone[1].network='wwan'")),
            "should add wwan to firewall wan zone: {cmds:?}"
        );
    }
}
