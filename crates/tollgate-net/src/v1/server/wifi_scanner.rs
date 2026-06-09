#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

//! WiFi radio scanner — parses iwinfo output for available networks.
//!
//! # Go v1 vs Rust comparison
//!
//! Go v1: `scanner.go` uses `os/exec` to run `iwinfo <radio> scan` and parses
//! the text output with string matching. No input validation.
//!
//! Rust: Same `iwinfo` approach (OpenWrt doesn't expose scan via ubus), but:
//! - Typed [`ScanResult`] instead of raw struct with string fields
//! - Proper error handling with `thiserror`
//! - Shell-safe: all radio names validated via [`validate_identifier`]
//! - Retry with exponential backoff (Go retries 3x with no backoff)
//! - Trait-based command execution for testability

use std::time::Duration;

use thiserror::Error;

use super::uci_ops::validate_identifier;

/// Errors from WiFi scanning operations.
#[derive(Debug, Error)]
pub enum WifiScanError {
    /// Invalid radio identifier (shell-unsafe characters).
    #[error("invalid radio identifier: {0}")]
    InvalidRadio(String),
    /// Shell command execution failed.
    #[error("command failed: {0}")]
    CommandFailed(String),
    /// Command timed out.
    #[error("command timed out after {0:?}")]
    Timeout(Duration),
    /// All retry attempts exhausted.
    #[error("scan failed after {attempts} attempts for radio {radio}: {last_error}")]
    RetriesExhausted {
        radio: String,
        attempts: u32,
        last_error: String,
    },
    /// No radios found on the system.
    #[error("no radios found")]
    NoRadios,
}

/// Encryption type detected from iwinfo output.
///
/// Maps to OpenWrt UCI encryption values. Go v1 uses `DetectEncryption()`
/// which returns a string; Rust uses a typed enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionType {
    /// No encryption / open network.
    None,
    /// WEP (deprecated, rarely seen).
    Wep,
    /// WPA-PSK.
    Psk,
    /// WPA2-PSK (most common for TollGate APs).
    Psk2,
    /// WPA2-PSK + CCMP.
    Psk2Ccmp,
    /// WPA3-SAE.
    Sae,
    /// WPA2/WPA3 mixed mode (SAE + PSK).
    SaeMixed,
    /// WPA2-Enterprise (EAP).
    Wpa2Eap,
    /// Unknown encryption type — stores the raw string for logging.
    Unknown(String),
}

impl std::fmt::Display for EncryptionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Wep => write!(f, "wep"),
            Self::Psk => write!(f, "psk"),
            Self::Psk2 => write!(f, "psk2"),
            Self::Psk2Ccmp => write!(f, "psk2-ccmp"),
            Self::Sae => write!(f, "sae"),
            Self::SaeMixed => write!(f, "sae-mixed"),
            Self::Wpa2Eap => write!(f, "wpa2-eap"),
            Self::Unknown(s) => write!(f, "unknown({s})"),
        }
    }
}

/// Encryption type mapped to UCI encryption value.
///
/// Returns the value suitable for `uci set wireless.<iface>.encryption=...`.
impl EncryptionType {
    /// Convert to UCI encryption string.
    ///
    /// Go v1 maps: none/open/wep → none, sae mixed → sae-mixed, sae → sae,
    /// wpa2 psk → psk2, wpa psk → psk, eap → wpa2-eap, default → psk2.
    #[must_use]
    pub fn to_uci_value(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Wep => "wep",
            Self::Psk => "psk",
            Self::Psk2 | Self::Psk2Ccmp | Self::Unknown(_) => "psk2",
            Self::Sae => "sae",
            Self::SaeMixed => "sae-mixed",
            Self::Wpa2Eap => "wpa2-eap",
        }
    }
}

/// A single scanned WiFi network.
///
/// Go v1 returns a struct with string fields for everything.
/// Rust uses typed fields: `i32` for signal, `u32` for channel,
/// and [`EncryptionType`] for encryption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// BSSID (MAC address) of the access point.
    pub bssid: String,
    /// SSID (network name). Empty for hidden networks.
    pub ssid: String,
    /// Signal strength in dBm (negative, e.g. -45).
    pub signal_dbm: i32,
    /// Encryption type.
    pub encryption: EncryptionType,
    /// Channel number.
    pub channel: u32,
    /// Radio that detected this network (e.g. "radio0").
    pub radio: String,
}

impl ScanResult {
    /// Quality score: signal strength normalized to 0-100.
    ///
    /// Go v1 doesn't calculate this; it just sorts by raw signal.
    /// Useful for comparing networks across different radios.
    #[must_use]
    pub fn quality(&self) -> u32 {
        // Typical range: -30 (excellent) to -90 (unusable)
        // Clamp to -90..-30 and map to 0..100
        let clamped = self.signal_dbm.clamp(-90, -30);
        let normalized = (clamped + 90) as u32;
        // -90 → 0, -30 → 60, scale to 0-100
        (normalized * 100) / 60
    }
}

/// Output from a command execution.
///
/// Abstraction over `tokio::process::Command` output for testability.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Exit code (0 = success).
    pub exit_code: i32,
    /// stdout content.
    pub stdout: String,
    /// stderr content.
    pub stderr: String,
}

impl CommandOutput {
    /// Whether the command succeeded (exit code 0).
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Trait for executing shell commands.
///
/// Production implementation uses `tokio::process::Command`.
/// Tests use a mock that returns canned output.
#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn execute(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<CommandOutput, WifiScanError>;
}

/// Production command executor using `tokio::process::Command`.
///
/// Uses `kill_on_drop(true)` and configurable timeout to prevent
/// zombie processes. Go v1 has no such safeguards.
pub struct SystemCommandExecutor {
    timeout: Duration,
}

impl SystemCommandExecutor {
    /// Create a new executor with the given command timeout.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[async_trait::async_trait]
impl CommandExecutor for SystemCommandExecutor {
    async fn execute(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<CommandOutput, WifiScanError> {
        let result = tokio::time::timeout(
            self.timeout,
            tokio::process::Command::new(program)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
            Ok(Err(e)) => Err(WifiScanError::CommandFailed(format!(
                "{program} {}: {e}",
                args.join(" ")
            ))),
            Err(_) => Err(WifiScanError::Timeout(self.timeout)),
        }
    }
}

/// WiFi radio scanner.
///
/// Scans all radios via `iwinfo`, parses output, returns typed results.
///
/// # Go v1 vs Rust
///
/// Go v1: `scanner.go` — `ScanAllRadios()`, `scanRadio()`, `ParseIwinfoOutput()`
/// All in one file, no abstraction, global functions.
///
/// Rust: Struct with trait-based command execution for testability.
/// Exponential backoff on retry (Go does 3x with no backoff).
pub struct WifiScanner {
    /// Timeout for each scan command.
    command_timeout: Duration,
    /// Maximum retry attempts per radio.
    max_retries: u32,
    /// Command executor (production or mock).
    executor: Box<dyn CommandExecutor>,
}

impl WifiScanner {
    /// Create a new scanner with default settings.
    ///
    /// Default timeout: 30s, max retries: 3.
    pub fn new() -> Self {
        Self {
            command_timeout: Duration::from_secs(30),
            max_retries: 3,
            executor: Box::new(SystemCommandExecutor::new(Duration::from_secs(30))),
        }
    }

    /// Create a scanner with a custom command executor (for testing).
    pub fn with_executor(executor: Box<dyn CommandExecutor>) -> Self {
        Self {
            command_timeout: Duration::from_secs(30),
            max_retries: 3,
            executor,
        }
    }

    /// Set a custom command timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Set the maximum number of retry attempts per radio.
    #[must_use]
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Scan all radios and return combined results sorted by signal (strongest first).
    ///
    /// Go v1: `ScanAllRadios()` — parallel scans all radios, sorts by signal desc.
    /// Rust: Sequential scans (parallel requires spawning tasks per radio,
    /// not worth the complexity for typically 1-2 radios).
    pub async fn scan_all_radios(&self) -> Result<Vec<ScanResult>, WifiScanError> {
        let radios = self.get_radios().await?;
        if radios.is_empty() {
            return Err(WifiScanError::NoRadios);
        }

        let mut all_results = Vec::new();
        for radio in &radios {
            match self.scan_radio(radio).await {
                Ok(results) => all_results.extend(results),
                Err(e) => {
                    tracing::warn!("scan failed for radio {radio}: {e}");
                    // Continue scanning other radios, same as Go v1
                }
            }
        }

        // Sort by signal strength, strongest first (Go v1 behavior)
        all_results.sort_by_key(|b| std::cmp::Reverse(b.signal_dbm));

        tracing::info!(
            "scanned {} radios, found {} networks",
            radios.len(),
            all_results.len()
        );

        Ok(all_results)
    }

    /// Scan a single radio with retry and exponential backoff.
    ///
    /// Go v1: `scanRadio()` retries 3x with no backoff.
    /// Rust: Retries with exponential backoff (1s, 2s, 4s).
    pub async fn scan_radio(&self, radio: &str) -> Result<Vec<ScanResult>, WifiScanError> {
        validate_identifier(radio).map_err(|e| WifiScanError::InvalidRadio(e.to_string()))?;

        let mut last_error = String::new();
        for attempt in 0..self.max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                tracing::debug!(
                    "retry {}/{} for radio {radio} after {delay:?}",
                    attempt + 1,
                    self.max_retries
                );
                tokio::time::sleep(delay).await;
            }

            match self
                .executor
                .execute("iwinfo", vec![radio.to_owned(), "scan".to_owned()])
                .await
            {
                Ok(output) if output.success() => {
                    let results = self.parse_iwinfo_output(&output.stdout, radio);
                    tracing::debug!(
                        "radio {radio}: found {} networks (attempt {})",
                        results.len(),
                        attempt + 1
                    );
                    return Ok(results);
                }
                Ok(output) => {
                    last_error =
                        format!("exit code {}: {}", output.exit_code, output.stderr.trim());
                    tracing::debug!(
                        "scan attempt {}/{} for radio {radio} failed: {last_error}",
                        attempt + 1,
                        self.max_retries
                    );
                }
                Err(e) => {
                    last_error = e.to_string();
                    tracing::debug!(
                        "scan attempt {}/{} for radio {radio} error: {last_error}",
                        attempt + 1,
                        self.max_retries
                    );
                }
            }
        }

        Err(WifiScanError::RetriesExhausted {
            radio: radio.to_owned(),
            attempts: self.max_retries,
            last_error,
        })
    }

    /// Detect available radios by parsing `/etc/config/wireless`.
    ///
    /// Go v1: `GetRadios()` parses `/etc/config/wireless` for `config wifi-device`.
    /// Rust: Uses `uci show wireless` command (more reliable than file parsing).
    pub async fn get_radios(&self) -> Result<Vec<String>, WifiScanError> {
        let output = self
            .executor
            .execute("uci", vec!["show".to_owned(), "wireless".to_owned()])
            .await
            .map_err(|e| WifiScanError::CommandFailed(format!("uci show wireless: {e}")))?;

        if !output.success() {
            return Err(WifiScanError::CommandFailed(format!(
                "uci show wireless: {}",
                output.stderr.trim()
            )));
        }

        // Parse "wireless.radio0=wifi-device" lines
        let mut radios = Vec::new();
        for line in output.stdout.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("wireless.") {
                if rest.contains("=wifi-device") {
                    if let Some(name) = rest.split('=').next() {
                        if validate_identifier(name).is_ok() {
                            radios.push(name.to_owned());
                        }
                    }
                }
            }
        }

        // Deduplicate and sort
        radios.sort();
        radios.dedup();

        tracing::debug!("detected {} radios: {:?}", radios.len(), radios);
        Ok(radios)
    }

    /// Parse `iwinfo <radio> scan` output into typed results.
    ///
    /// Go v1: `ParseIwinfoOutput(output, radio)` — splits by "Cell",
    /// extracts Address/ESSID/Signal/Encryption/Channel via string matching.
    ///
    /// Rust: Same approach but with proper error handling and skipping
    /// hidden SSIDs (empty ESSID).
    #[allow(clippy::similar_names)]
    pub fn parse_iwinfo_output(&self, output: &str, radio: &str) -> Vec<ScanResult> {
        let mut results = Vec::new();

        // iwinfo output is structured as sections separated by empty lines
        // Each section starts with "Cell" or just has key: value pairs
        let mut current_bssid = String::new();
        let mut current_ssid = String::new();
        let mut current_signal: i32 = -100;
        let mut current_encryption = EncryptionType::None;
        let mut current_channel: u32 = 0;

        for line in output.lines() {
            let line = line.trim();

            // New cell starts — flush previous result
            if line.starts_with("Cell") {
                if !current_ssid.is_empty() && !current_bssid.is_empty() {
                    results.push(ScanResult {
                        bssid: current_bssid.clone(),
                        ssid: current_ssid.clone(),
                        signal_dbm: current_signal,
                        encryption: current_encryption.clone(),
                        channel: current_channel,
                        radio: radio.to_owned(),
                    });
                }
                // Reset for new cell
                current_bssid = String::new();
                current_ssid = String::new();
                current_signal = -100;
                current_encryption = EncryptionType::None;
                current_channel = 0;

                // Parse BSSID from "Cell 01 - Address: AA:BB:CC:DD:EE:FF"
                if let Some(addr) = line.split("Address:").nth(1) {
                    addr.trim().clone_into(&mut current_bssid);
                }
                continue;
            }

            // Parse ESSID
            if let Some(essid) = line.strip_prefix("ESSID:") {
                let essid = essid.trim();
                essid
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(essid)
                    .clone_into(&mut current_ssid);
            }

            // Parse signal: "Quality: 70/100  Signal: -45 dBm"
            // or just "Signal: -45 dBm"
            if line.contains("Signal:") {
                if let Some(signal_part) = line.split("Signal:").nth(1) {
                    let signal_str = signal_part.trim();
                    // Extract the number before "dBm"
                    if let Some(num_part) = signal_str.split("dBm").next() {
                        if let Ok(signal) = num_part.trim().parse::<i32>() {
                            current_signal = signal;
                        }
                    }
                }
            }

            // Parse encryption
            if line.starts_with("Encryption:") {
                let enc_str = line.strip_prefix("Encryption:").unwrap_or("").trim();
                current_encryption = self.detect_encryption(enc_str);
            }

            // Parse channel — may be inline "Mode: Master  Channel: 36" or separate "Channel: 36"
            if line.contains("Channel:") {
                if let Some(ch_part) = line.split("Channel:").nth(1) {
                    let ch_str = ch_part.trim();
                    if let Ok(ch) = ch_str.parse::<u32>() {
                        current_channel = ch;
                    }
                }
            }
        }

        // Flush last cell
        if !current_ssid.is_empty() && !current_bssid.is_empty() {
            results.push(ScanResult {
                bssid: current_bssid,
                ssid: current_ssid,
                signal_dbm: current_signal,
                encryption: current_encryption,
                channel: current_channel,
                radio: radio.to_owned(),
            });
        }

        // Sort by signal, strongest first
        results.sort_by_key(|b| std::cmp::Reverse(b.signal_dbm));

        results
    }

    /// Detect encryption type from iwinfo encryption string.
    ///
    /// Go v1: `DetectEncryption(encStr)` — maps strings to UCI values.
    /// Maps: none/open/wep → none, sae mixed → sae-mixed, sae → sae,
    /// wpa2 psk → psk2, wpa psk → psk, eap → wpa2-eap, default → psk2.
    ///
    /// Rust: Same mapping but returns typed [`EncryptionType`].
    #[must_use]
    pub fn detect_encryption(&self, enc_str: &str) -> EncryptionType {
        let lower = enc_str.to_lowercase();

        // Check for none/open first
        if lower.contains("none") || lower.contains("open") {
            return EncryptionType::None;
        }

        // WEP
        if lower.contains("wep") {
            return EncryptionType::Wep;
        }

        // WPA3 SAE mixed (must check before plain SAE)
        if lower.contains("sae")
            && (lower.contains("mixed") || lower.contains("wpa2") || lower.contains("wpa3"))
        {
            return EncryptionType::SaeMixed;
        }

        // WPA3 SAE only
        if lower.contains("sae") {
            return EncryptionType::Sae;
        }

        // WPA2-EAP (Enterprise)
        if lower.contains("eap") || lower.contains("802.1x") {
            return EncryptionType::Wpa2Eap;
        }

        // WPA2-PSK
        if lower.contains("wpa2") && lower.contains("psk") {
            if lower.contains("ccmp") {
                return EncryptionType::Psk2Ccmp;
            }
            return EncryptionType::Psk2;
        }

        // WPA-PSK (WPA1)
        if lower.contains("wpa") && lower.contains("psk") {
            return EncryptionType::Psk;
        }

        // WPA2 without PSK — assume PSK2 (same as Go default)
        if lower.contains("wpa2") {
            return EncryptionType::Psk2;
        }

        // WPA without PSK
        if lower.contains("wpa") {
            return EncryptionType::Psk;
        }

        // Unknown — return the raw string for logging
        if enc_str.is_empty() {
            return EncryptionType::None;
        }

        EncryptionType::Unknown(enc_str.to_owned())
    }

    /// Find the best radio for a given SSID.
    ///
    /// Returns the radio name that sees the strongest signal for the SSID,
    /// or `None` if the SSID is not found on any radio.
    ///
    /// Go v1: `FindBestRadioForSSID()` — returns first matching SSID's radio.
    /// Rust: Returns the radio with the strongest signal (more correct).
    pub async fn find_best_radio_for_ssid(
        &self,
        ssid: &str,
    ) -> Result<Option<String>, WifiScanError> {
        let results = self.scan_all_radios().await?;
        let best = results
            .iter()
            .filter(|r| r.ssid == ssid)
            .max_by_key(|r| r.signal_dbm)
            .map(|r| r.radio.clone());
        Ok(best)
    }
}

impl Default for WifiScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample iwinfo output with multiple cells.
    const SAMPLE_IWINFO_OUTPUT: &str = r#"Cell 01 - Address: AA:BB:CC:DD:EE:01
          ESSID: "TollGate-5G"
          Mode: Master  Channel: 36
          Signal: -45 dBm  Quality: 70/100
          Encryption: WPA2 PSK (CCMP)
Cell 02 - Address: AA:BB:CC:DD:EE:02
          ESSID: "TollGate-2G"
          Mode: Master  Channel: 1
          Signal: -67 dBm  Quality: 40/100
          Encryption: WPA2 PSK (CCMP)
Cell 03 - Address: AA:BB:CC:DD:EE:03
          ESSID: "OpenWrt"
          Mode: Master  Channel: 6
          Signal: -80 dBm  Quality: 20/100
          Encryption: none
Cell 04 - Address: AA:BB:CC:DD:EE:04
          ESSID: ""
          Mode: Master  Channel: 11
          Signal: -55 dBm  Quality: 55/100
          Encryption: WPA2 PSK (CCMP)
"#;

    #[test]
    fn test_parse_iwinfo_output_basic() {
        let scanner = WifiScanner::new();
        let results = scanner.parse_iwinfo_output(SAMPLE_IWINFO_OUTPUT, "radio0");

        // Should have 3 results (hidden SSID skipped)
        assert_eq!(results.len(), 3);

        // First result should be strongest signal
        assert_eq!(results[0].ssid, "TollGate-5G");
        assert_eq!(results[0].signal_dbm, -45);
        assert_eq!(results[0].channel, 36);
        assert_eq!(results[0].bssid, "AA:BB:CC:DD:EE:01");
        assert_eq!(results[0].encryption, EncryptionType::Psk2Ccmp);
        assert_eq!(results[0].radio, "radio0");

        // Second result
        assert_eq!(results[1].ssid, "TollGate-2G");
        assert_eq!(results[1].signal_dbm, -67);
        assert_eq!(results[1].channel, 1);

        // Third result (open network)
        assert_eq!(results[2].ssid, "OpenWrt");
        assert_eq!(results[2].signal_dbm, -80);
        assert_eq!(results[2].encryption, EncryptionType::None);
    }

    #[test]
    fn test_parse_iwinfo_output_hidden_ssid() {
        let scanner = WifiScanner::new();
        let results = scanner.parse_iwinfo_output(SAMPLE_IWINFO_OUTPUT, "radio0");

        // Cell 04 has empty ESSID — should be skipped
        let hidden = results.iter().find(|r| r.bssid == "AA:BB:CC:DD:EE:04");
        assert!(hidden.is_none(), "hidden SSID should be skipped");
    }

    #[test]
    fn test_parse_iwinfo_output_empty() {
        let scanner = WifiScanner::new();
        let results = scanner.parse_iwinfo_output("", "radio0");
        assert!(results.is_empty());

        let results = scanner.parse_iwinfo_output("No scan results", "radio0");
        assert!(results.is_empty());
    }

    #[test]
    fn test_detect_encryption_all_types() {
        let scanner = WifiScanner::new();

        // None / open
        assert_eq!(scanner.detect_encryption("none"), EncryptionType::None);
        assert_eq!(scanner.detect_encryption("open"), EncryptionType::None);

        // WEP
        assert_eq!(scanner.detect_encryption("WEP"), EncryptionType::Wep);

        // WPA-PSK
        assert_eq!(
            scanner.detect_encryption("WPA PSK (TKIP)"),
            EncryptionType::Psk
        );

        // WPA2-PSK
        assert_eq!(
            scanner.detect_encryption("WPA2 PSK (CCMP)"),
            EncryptionType::Psk2Ccmp
        );

        // WPA2-PSK without CCMP
        assert_eq!(scanner.detect_encryption("WPA2 PSK"), EncryptionType::Psk2);

        // SAE mixed (WPA2/WPA3)
        assert_eq!(
            scanner.detect_encryption("SAE mixed (CCMP)"),
            EncryptionType::SaeMixed
        );

        // SAE (WPA3 only)
        assert_eq!(scanner.detect_encryption("SAE (CCMP)"), EncryptionType::Sae);

        // WPA2-EAP (Enterprise)
        assert_eq!(
            scanner.detect_encryption("WPA2 802.1X (CCMP)"),
            EncryptionType::Wpa2Eap
        );

        // EAP
        assert_eq!(
            scanner.detect_encryption("WPA2 EAP (CCMP)"),
            EncryptionType::Wpa2Eap
        );
    }

    #[test]
    fn test_detect_encryption_unknown() {
        let scanner = WifiScanner::new();

        let result = scanner.detect_encryption("some-future-encryption");
        assert!(matches!(result, EncryptionType::Unknown(ref s) if s == "some-future-encryption"));
    }

    #[test]
    fn test_detect_encryption_empty_is_none() {
        let scanner = WifiScanner::new();
        assert_eq!(scanner.detect_encryption(""), EncryptionType::None);
    }

    #[test]
    fn test_encryption_to_uci_value() {
        assert_eq!(EncryptionType::None.to_uci_value(), "none");
        assert_eq!(EncryptionType::Wep.to_uci_value(), "wep");
        assert_eq!(EncryptionType::Psk.to_uci_value(), "psk");
        assert_eq!(EncryptionType::Psk2.to_uci_value(), "psk2");
        assert_eq!(EncryptionType::Psk2Ccmp.to_uci_value(), "psk2");
        assert_eq!(EncryptionType::Sae.to_uci_value(), "sae");
        assert_eq!(EncryptionType::SaeMixed.to_uci_value(), "sae-mixed");
        assert_eq!(EncryptionType::Wpa2Eap.to_uci_value(), "wpa2-eap");
        assert_eq!(
            EncryptionType::Unknown("foo".to_owned()).to_uci_value(),
            "psk2"
        );
    }

    #[test]
    fn test_encryption_display() {
        assert_eq!(EncryptionType::None.to_string(), "none");
        assert_eq!(EncryptionType::Psk2.to_string(), "psk2");
        assert_eq!(
            EncryptionType::Unknown("foo".to_owned()).to_string(),
            "unknown(foo)"
        );
    }

    #[test]
    fn test_scan_result_quality() {
        let result = ScanResult {
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            ssid: "Test".to_owned(),
            signal_dbm: -30,
            encryption: EncryptionType::Psk2,
            channel: 36,
            radio: "radio0".to_owned(),
        };
        assert_eq!(result.quality(), 100); // -30 → 100%

        let weak = ScanResult {
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            ssid: "Test".to_owned(),
            signal_dbm: -90,
            encryption: EncryptionType::Psk2,
            channel: 36,
            radio: "radio0".to_owned(),
        };
        assert_eq!(weak.quality(), 0); // -90 → 0%
    }

    #[test]
    fn test_command_output_success() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "hello".to_owned(),
            stderr: String::new(),
        };
        assert!(output.success());

        let failed = CommandOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error".to_owned(),
        };
        assert!(!failed.success());
    }

    /// Mock-based test for get_radios parsing.
    #[tokio::test]
    async fn test_get_radios_parses_uci_output() {
        let mut mock = MockCommandExecutor::new();
        mock.expect_execute()
            .withf(|program, args| program == "uci" && args.iter().map(String::as_str).collect::<Vec<_>>() == ["show", "wireless"])
            .returning(|_, _| {
                Ok(CommandOutput {
                    exit_code: 0,
                    stdout: "wireless.radio0=wifi-device\nwireless.radio0.channel='36'\nwireless.default_radio0=wifi-iface\nwireless.radio1=wifi-device\n".to_owned(),
                    stderr: String::new(),
                })
            });

        let scanner = WifiScanner::with_executor(Box::new(mock));
        let radios = scanner.get_radios().await.unwrap();
        assert_eq!(radios, vec!["radio0", "radio1"]);
    }

    /// Mock-based test for scan_radio with valid output.
    #[tokio::test]
    async fn test_scan_radio_success() {
        let mut mock = MockCommandExecutor::new();
        mock.expect_execute()
            .withf(|program, args| {
                program == "iwinfo"
                    && args.iter().map(String::as_str).collect::<Vec<_>>() == ["radio0", "scan"]
            })
            .returning(|_, _| {
                Ok(CommandOutput {
                    exit_code: 0,
                    stdout: r#"Cell 01 - Address: AA:BB:CC:DD:EE:01
          ESSID: "TestNet"
          Mode: Master  Channel: 36
          Signal: -50 dBm  Quality: 60/100
          Encryption: WPA2 PSK (CCMP)
"#
                    .to_owned(),
                    stderr: String::new(),
                })
            });

        let scanner = WifiScanner::with_executor(Box::new(mock));
        let results = scanner.scan_radio("radio0").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ssid, "TestNet");
        assert_eq!(results[0].signal_dbm, -50);
    }

    /// Test that invalid radio identifiers are rejected.
    #[tokio::test]
    async fn test_scan_radio_invalid_identifier() {
        let scanner = WifiScanner::new();
        let result = scanner.scan_radio("radio;rm -rf /").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WifiScanError::InvalidRadio(_)
        ));
    }

    /// Test retry behavior with mock that fails then succeeds.
    #[tokio::test]
    async fn test_scan_radio_retries_then_succeeds() {
        let mut mock = MockCommandExecutor::new();
        // First two calls fail, third succeeds
        mock.expect_execute().times(2).returning(|_, _| {
            Ok(CommandOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "device busy".to_owned(),
            })
        });
        mock.expect_execute().returning(|_, _| {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: r#"Cell 01 - Address: AA:BB:CC:DD:EE:01
          ESSID: "RetryNet"
          Signal: -55 dBm
          Encryption: none
"#
                .to_owned(),
                stderr: String::new(),
            })
        });

        let scanner = WifiScanner::with_executor(Box::new(mock)).with_max_retries(3);
        let results = scanner.scan_radio("radio0").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ssid, "RetryNet");
    }

    /// Test that retries exhaust when all attempts fail.
    #[tokio::test]
    async fn test_scan_radio_retries_exhausted() {
        let mut mock = MockCommandExecutor::new();
        mock.expect_execute().returning(|_, _| {
            Ok(CommandOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "device not found".to_owned(),
            })
        });

        let scanner = WifiScanner::with_executor(Box::new(mock)).with_max_retries(2);
        let result = scanner.scan_radio("radio0").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            WifiScanError::RetriesExhausted { attempts, .. } => {
                assert_eq!(attempts, 2);
            }
            e => panic!("expected RetriesExhausted, got {e:?}"),
        }
    }
}
