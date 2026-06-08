#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

//! Automatic upstream WiFi selection with hysteresis, blacklist, and circuit breaker.
//!
//! # Go v1 vs Rust comparison
//!
//! Go v1: `upstream_manager.go` uses ad-hoc if/else with mutable state scattered
//! across multiple methods. Circuit breaker is a simple counter. Blacklist is
//! a `map[string]time.Time`. No state machine — just boolean flags.
//!
//! Rust improvements:
//! - Enum-based state machine (`Disconnected → Connected → Switching`)
//! - Typed blacklist with TTL (not `map[string]time.Time`)
//! - Structured circuit breaker with configurable thresholds
//! - All configuration in one typed struct with defaults
//! - Candidate selection supports both normal and reseller mode

use std::collections::HashMap;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::wifi_connector::WifiConnector;
use super::wifi_scanner::{EncryptionType, ScanResult, WifiScanner};

/// Vendor element processor for WiFi network scoring (Go v1 parity).
///
/// Go v1: `vendor_element_manager.go` — `VendorElementProcessor` with
/// `ExtractAndScore`, `calculateScore`, `SetLocalAPVendorElements`,
/// `GetLocalAPVendorElements`. Vendor element parsing is commented out
/// in Go; scoring only checks SSID prefix.
///
/// Rust: Same stubs, same scoring formula — signal strength + 100 for
/// TollGate-prefixed SSIDs. Vendor element parsing remains stubbed.
pub struct VendorElementProcessor;

impl VendorElementProcessor {
    /// Create a new vendor element processor.
    pub fn new() -> Self {
        Self
    }

    /// Score a scanned network for upstream selection priority.
    ///
    /// Mirrors Go v1's `VendorElementProcessor.ExtractAndScore`.
    /// Currently: signal strength + 100 for TollGate- prefixed SSIDs.
    pub fn extract_and_score(&self, network: &ScanResult) -> (serde_json::Value, i32) {
        let vendor_elements = serde_json::json!({});
        let score = self.calculate_score(network, &vendor_elements);
        (vendor_elements, score)
    }

    fn calculate_score(&self, network: &ScanResult, _vendor_elements: &serde_json::Value) -> i32 {
        let mut score = network.signal_dbm;
        if network.ssid.starts_with("TollGate-") {
            score += 100;
        }
        score
    }

    /// Set vendor elements on local AP (stubbed — matches Go v1).
    pub fn set_local_ap_vendor_elements(
        &self,
        _elements: &HashMap<String, String>,
    ) -> Result<(), String> {
        tracing::debug!("SetLocalAPVendorElements called (stubbed — matches Go v1)");
        Ok(())
    }

    /// Get vendor elements from local AP (stubbed — matches Go v1).
    pub fn get_local_ap_vendor_elements(&self) -> Result<HashMap<String, String>, String> {
        tracing::debug!("GetLocalAPVendorElements called (stubbed — matches Go v1)");
        Ok(HashMap::new())
    }
}

impl Default for VendorElementProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from upstream manager operations.
#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("scan failed: {0}")]
    ScanFailed(#[from] super::wifi_scanner::WifiScanError),
    #[error("connect failed: {0}")]
    ConnectFailed(#[from] super::wifi_connector::WifiConnectError),
    #[error("no candidates found")]
    NoCandidates,
    #[error("circuit breaker open — cooldown until {until:?}")]
    CircuitOpen { until: Instant },
    #[error("manager is in cooldown until {until:?}")]
    InCooldown { until: Instant },
}

/// State machine for the upstream manager.
///
/// Go v1 uses boolean flags (`isConnected`, `isSwitching`, `inCooldown`).
/// Rust uses a typed enum — impossible to be in two states simultaneously.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerState {
    /// No upstream connection.
    Disconnected,
    /// Currently connected to an upstream AP.
    Connected {
        iface: String,
        ssid: String,
        signal_dbm: i32,
        radio: String,
    },
    /// In the process of switching from one AP to another.
    Switching { from_ssid: String, to_ssid: String },
    /// Cooling down after too many failures.
    Cooldown { until: Instant },
}

/// Configuration for [`UpstreamManager`].
///
/// Maps to Go's `ManagerConfig` but with typed durations and documented defaults.
#[derive(Debug, Clone)]
pub struct UpstreamManagerConfig {
    /// How often to scan for better upstreams.
    pub scan_interval: Duration,
    /// How often to check current connection quality (fast path).
    pub fast_check_interval: Duration,
    /// Minimum signal improvement (dB) to trigger a switch.
    pub hysteresis_db: i32,
    /// Switch regardless of hysteresis if signal is below this floor.
    pub signal_floor_dbm: i32,
    /// How long a blacklisted SSID stays blacklisted.
    pub blacklist_ttl: Duration,
    /// Consecutive failures before opening the circuit breaker.
    pub max_consecutive_failures: u32,
    /// How long to cool down after the circuit breaker opens.
    pub cooldown_duration: Duration,
    /// Grace period at startup before scanning begins.
    pub startup_grace_period: Duration,
    /// In reseller mode, prefer TollGate SSIDs over non-TollGate.
    pub reseller_mode: bool,
    /// SSID prefix that identifies TollGate networks (for reseller mode).
    pub tollgate_ssid_prefix: String,
}

impl Default for UpstreamManagerConfig {
    fn default() -> Self {
        Self {
            scan_interval: Duration::from_secs(300),
            fast_check_interval: Duration::from_secs(30),
            hysteresis_db: 12,
            signal_floor_dbm: -85,
            blacklist_ttl: Duration::from_secs(3600),
            max_consecutive_failures: 3,
            cooldown_duration: Duration::from_secs(600),
            startup_grace_period: Duration::from_secs(90),
            reseller_mode: false,
            tollgate_ssid_prefix: "TollGate".to_owned(),
        }
    }
}

/// A candidate for switching upstream.
#[derive(Debug, Clone)]
pub struct SwitchCandidate {
    pub ssid: String,
    pub bssid: String,
    pub signal_dbm: i32,
    pub radio: String,
    pub channel: u32,
    pub encryption: EncryptionType,
    pub is_tollgate: bool,
}

impl From<&ScanResult> for SwitchCandidate {
    fn from(scan: &ScanResult) -> Self {
        Self {
            ssid: scan.ssid.clone(),
            bssid: scan.bssid.clone(),
            signal_dbm: scan.signal_dbm,
            radio: scan.radio.clone(),
            channel: scan.channel,
            encryption: scan.encryption.clone(),
            is_tollgate: false,
        }
    }
}

/// Why a scan cycle was triggered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScanReason {
    /// Periodic full scan.
    Periodic,
    /// Fast check of current connection quality.
    FastCheck,
    /// Emergency scan (current connection lost or critically weak).
    Emergency,
    /// First scan after startup.
    Startup,
}

/// Result of a scan cycle.
#[derive(Debug, Clone)]
pub enum ScanCycleResult {
    /// No switch needed — current connection is fine.
    NoSwitchNeeded { current_signal_dbm: i32 },
    /// Switched to a new upstream.
    Switched {
        from_ssid: String,
        to_ssid: String,
        to_signal_dbm: i32,
    },
    /// Disconnected — no suitable upstream found.
    Disconnected,
    /// Still in cooldown.
    InCooldown { until: Instant },
}

/// Blacklist for SSIDs that failed connection.
///
/// Go v1 uses `map[string]time.Time` with manual expiry checks.
/// Rust uses a typed struct with `purge()` for cleanup.
#[derive(Debug, Clone)]
pub struct Blacklist {
    entries: HashMap<String, Instant>,
    ttl: Duration,
}

impl Blacklist {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    pub fn add(&mut self, ssid: &str) {
        self.entries
            .insert(ssid.to_owned(), Instant::now() + self.ttl);
        tracing::debug!(ssid = %ssid, ttl = ?self.ttl, "SSID blacklisted");
    }

    pub fn is_blacklisted(&self, ssid: &str) -> bool {
        self.entries
            .get(ssid)
            .is_some_and(|expires_at| Instant::now() < *expires_at)
    }

    pub fn purge(&mut self) {
        let now = Instant::now();
        let before = self.entries.len();
        self.entries.retain(|_, expires_at| *expires_at > now);
        let removed = before - self.entries.len();
        if removed > 0 {
            tracing::debug!("purged {removed} expired blacklist entries");
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Circuit breaker for connection failures.
///
/// Go v1 uses a simple `consecutiveFailures int` counter.
/// Rust uses a typed struct with configurable thresholds and cooldown.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    failures: u32,
    max_failures: u32,
    cooldown_until: Option<Instant>,
    cooldown_duration: Duration,
}

impl CircuitBreaker {
    pub fn new(max_failures: u32, cooldown_duration: Duration) -> Self {
        Self {
            failures: 0,
            max_failures,
            cooldown_until: None,
            cooldown_duration,
        }
    }

    pub fn record_success(&mut self) {
        self.failures = 0;
        self.cooldown_until = None;
    }

    /// Returns `true` if the circuit just opened (threshold reached).
    pub fn record_failure(&mut self) -> bool {
        self.failures += 1;
        if self.failures >= self.max_failures {
            self.cooldown_until = Some(Instant::now() + self.cooldown_duration);
            tracing::warn!(
                failures = self.failures,
                cooldown = ?self.cooldown_duration,
                "Circuit breaker opened"
            );
            true
        } else {
            false
        }
    }

    pub fn is_open(&self) -> bool {
        self.cooldown_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn remaining_cooldown(&self) -> Option<Duration> {
        self.cooldown_until
            .map(|until| until.duration_since(Instant::now()).max(Duration::ZERO))
    }
}

/// Automatic upstream WiFi manager.
///
/// Ties together [`WifiScanner`] and [`WifiConnector`] with:
/// - Hysteresis (don't switch unless significantly better)
/// - Blacklist (avoid recently-failed SSIDs)
/// - Circuit breaker (stop trying after repeated failures)
/// - Reseller mode (prefer TollGate SSIDs)
///
/// Go v1: `upstream_manager.go` — ~800 lines with scattered state.
/// Rust: Structured state machine, ~400 lines.
pub struct UpstreamManager {
    config: UpstreamManagerConfig,
    state: ManagerState,
    scanner: WifiScanner,
    connector: WifiConnector,
    blacklist: Blacklist,
    circuit_breaker: CircuitBreaker,
    pub vendor_processor: VendorElementProcessor,
}

impl UpstreamManager {
    pub fn new(config: UpstreamManagerConfig) -> Self {
        let blacklist = Blacklist::new(config.blacklist_ttl);
        let circuit_breaker =
            CircuitBreaker::new(config.max_consecutive_failures, config.cooldown_duration);

        Self {
            state: ManagerState::Disconnected,
            scanner: WifiScanner::new(),
            connector: WifiConnector::new(),
            blacklist,
            circuit_breaker,
            vendor_processor: VendorElementProcessor::new(),
            config,
        }
    }

    pub fn state(&self) -> &ManagerState {
        &self.state
    }

    pub fn blacklist(&self) -> &Blacklist {
        &self.blacklist
    }

    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }

    /// Run a single scan cycle and decide whether to switch.
    ///
    /// This is the main decision loop. Go v1 scatters this logic across
    /// `runManager()`, `evaluateUpstream()`, and `performSwitch()`.
    /// Rust consolidates it into one method with clear return types.
    pub async fn run_scan_cycle(
        &mut self,
        reason: ScanReason,
        password: &str,
    ) -> Result<ScanCycleResult, UpstreamError> {
        self.blacklist.purge();

        if self.circuit_breaker.is_open() {
            if let Some(until) = self.circuit_breaker.remaining_cooldown() {
                tracing::debug!(?until, "Circuit breaker open, skipping scan");
                self.state = ManagerState::Cooldown {
                    until: Instant::now() + until,
                };
                return Ok(ScanCycleResult::InCooldown {
                    until: Instant::now() + until,
                });
            }
            tracing::info!("Circuit breaker cooldown expired, resuming scans");
            self.circuit_breaker.record_success();
        }

        let networks = self.scanner.scan_all_radios().await?;

        let current_signal = match &self.state {
            ManagerState::Connected { signal_dbm, .. } => Some(*signal_dbm),
            _ => None,
        };

        let is_emergency = reason == ScanReason::Emergency
            || current_signal.is_some_and(|s| s < self.config.signal_floor_dbm);

        let candidates = self.find_candidates(&networks, is_emergency);

        if candidates.is_empty() {
            if current_signal.is_none() {
                self.state = ManagerState::Disconnected;
                return Ok(ScanCycleResult::Disconnected);
            }
            return Ok(ScanCycleResult::NoSwitchNeeded {
                current_signal_dbm: current_signal.unwrap_or(0),
            });
        }

        let best = &candidates[0];

        match &self.state {
            ManagerState::Disconnected => {
                tracing::info!(
                    ssid = %best.ssid,
                    signal = best.signal_dbm,
                    "Connecting to best candidate"
                );
                match self.try_connect(best, password).await {
                    Ok(iface) => {
                        self.circuit_breaker.record_success();
                        self.state = ManagerState::Connected {
                            iface,
                            ssid: best.ssid.clone(),
                            signal_dbm: best.signal_dbm,
                            radio: best.radio.clone(),
                        };
                        Ok(ScanCycleResult::Switched {
                            from_ssid: String::new(),
                            to_ssid: best.ssid.clone(),
                            to_signal_dbm: best.signal_dbm,
                        })
                    }
                    Err(e) => {
                        self.blacklist.add(&best.ssid);
                        let opened = self.circuit_breaker.record_failure();
                        if opened {
                            self.state = ManagerState::Cooldown {
                                until: Instant::now() + self.config.cooldown_duration,
                            };
                        }
                        Err(e)
                    }
                }
            }
            ManagerState::Connected {
                iface,
                ssid,
                signal_dbm,
                radio,
            } => {
                if !self.should_switch(*signal_dbm, best) {
                    return Ok(ScanCycleResult::NoSwitchNeeded {
                        current_signal_dbm: *signal_dbm,
                    });
                }

                tracing::info!(
                    from_ssid = %ssid,
                    from_signal = signal_dbm,
                    to_ssid = %best.ssid,
                    to_signal = best.signal_dbm,
                    "Switching upstream"
                );

                let from_ssid = ssid.clone();
                let from_radio = radio.clone();

                match self
                    .try_switch(&from_radio, &from_ssid, best, password)
                    .await
                {
                    Ok(()) => {
                        self.circuit_breaker.record_success();
                        self.state = ManagerState::Connected {
                            iface: iface.clone(),
                            ssid: best.ssid.clone(),
                            signal_dbm: best.signal_dbm,
                            radio: best.radio.clone(),
                        };
                        Ok(ScanCycleResult::Switched {
                            from_ssid,
                            to_ssid: best.ssid.clone(),
                            to_signal_dbm: best.signal_dbm,
                        })
                    }
                    Err(e) => {
                        self.blacklist.add(&best.ssid);
                        let opened = self.circuit_breaker.record_failure();
                        if opened {
                            self.state = ManagerState::Cooldown {
                                until: Instant::now() + self.config.cooldown_duration,
                            };
                        }
                        Err(e)
                    }
                }
            }
            ManagerState::Switching { .. } => {
                tracing::warn!("Scan cycle triggered while already switching — ignoring");
                Ok(ScanCycleResult::NoSwitchNeeded {
                    current_signal_dbm: 0,
                })
            }
            ManagerState::Cooldown { until } => Ok(ScanCycleResult::InCooldown { until: *until }),
        }
    }

    /// Find and rank candidates from scan results.
    ///
    /// In reseller mode, TollGate SSIDs are preferred over non-TollGate
    /// regardless of signal strength (within the signal floor).
    ///
    /// Go v1: `findBestCandidate()` — iterates with if/else checks.
    /// Rust: Filter + sort with explicit priority.
    pub fn find_candidates(
        &self,
        networks: &[ScanResult],
        is_emergency: bool,
    ) -> Vec<SwitchCandidate> {
        let mut candidates: Vec<SwitchCandidate> = networks
            .iter()
            .filter(|n| !n.ssid.is_empty())
            .filter(|n| !self.blacklist.is_blacklisted(&n.ssid))
            .map(|scan| {
                let mut candidate = SwitchCandidate::from(scan);
                candidate.is_tollgate = scan.ssid.starts_with(&self.config.tollgate_ssid_prefix);
                candidate
            })
            .filter(|c| is_emergency || c.signal_dbm >= self.config.signal_floor_dbm)
            .collect();

        if self.config.reseller_mode {
            candidates.sort_by(|a, b| {
                let score_a = a.signal_dbm + i32::from(a.ssid.starts_with("TollGate-")) * 100;
                let score_b = b.signal_dbm + i32::from(b.ssid.starts_with("TollGate-")) * 100;
                score_b.cmp(&score_a)
            });
        } else {
            candidates.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm));
        }

        candidates
    }

    /// Check if the candidate is good enough to trigger a switch.
    ///
    /// Go v1: `shouldSwitch()` — checks hysteresis and signal floor.
    /// Rust: Same logic but as a pure function.
    pub fn should_switch(&self, current_signal_dbm: i32, candidate: &SwitchCandidate) -> bool {
        if current_signal_dbm < self.config.signal_floor_dbm {
            tracing::debug!(
                current = current_signal_dbm,
                floor = self.config.signal_floor_dbm,
                "Below signal floor — switching regardless of hysteresis"
            );
            return true;
        }

        let improvement = candidate.signal_dbm - current_signal_dbm;
        if improvement >= self.config.hysteresis_db {
            tracing::debug!(
                improvement,
                hysteresis = self.config.hysteresis_db,
                "Hysteresis threshold met"
            );
            return true;
        }

        false
    }

    async fn try_connect(
        &self,
        candidate: &SwitchCandidate,
        password: &str,
    ) -> Result<String, UpstreamError> {
        self.connector
            .connect(
                &candidate.radio,
                &candidate.ssid,
                password,
                &candidate.encryption,
            )
            .await
            .map_err(UpstreamError::from)
    }

    async fn try_switch(
        &self,
        from_radio: &str,
        from_ssid: &str,
        candidate: &SwitchCandidate,
        password: &str,
    ) -> Result<(), UpstreamError> {
        self.connector
            .switch_upstream(
                from_radio,
                from_ssid,
                &candidate.radio,
                &candidate.ssid,
                password,
                &candidate.encryption,
            )
            .await
            .map_err(UpstreamError::from)
    }

    /// Mark the current connection as lost and transition to Disconnected.
    pub fn mark_disconnected(&mut self) {
        if let ManagerState::Connected { ssid, .. } = &self.state {
            tracing::warn!(ssid = %ssid, "Connection lost");
        }
        self.state = ManagerState::Disconnected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blacklist_add_and_check() {
        let mut bl = Blacklist::new(Duration::from_secs(60));
        bl.add("BadNet");
        assert!(bl.is_blacklisted("BadNet"));
        assert!(!bl.is_blacklisted("GoodNet"));
    }

    #[test]
    fn test_blacklist_ttl_expired() {
        let mut bl = Blacklist::new(Duration::from_millis(50));
        bl.add("ExpiredNet");
        assert!(bl.is_blacklisted("ExpiredNet"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!bl.is_blacklisted("ExpiredNet"));
    }

    #[test]
    fn test_blacklist_purge_removes_expired() {
        let mut bl = Blacklist::new(Duration::from_millis(50));
        bl.add("Old");
        bl.add("AlsoOld");
        std::thread::sleep(Duration::from_millis(100));
        bl.purge();
        assert!(bl.is_empty());
    }

    #[test]
    fn test_blacklist_purge_keeps_active() {
        let mut bl = Blacklist::new(Duration::from_secs(60));
        bl.add("Active");
        bl.purge();
        assert_eq!(bl.len(), 1);
    }

    #[test]
    fn test_circuit_breaker_stays_closed_on_success() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(10));
        cb.record_success();
        cb.record_success();
        assert!(!cb.is_open());
        assert_eq!(cb.failures, 0);
    }

    #[test]
    fn test_circuit_breaker_opens_after_max_failures() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(10));
        assert!(!cb.record_failure());
        assert!(!cb.record_failure());
        assert!(cb.record_failure());
        assert!(cb.is_open());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let mut cb = CircuitBreaker::new(2, Duration::from_secs(10));
        cb.record_failure();
        cb.record_success();
        assert_eq!(cb.failures, 0);
        assert!(!cb.is_open());
    }

    #[test]
    fn test_should_switch_with_hysteresis() {
        let config = UpstreamManagerConfig::default();
        let mgr = UpstreamManager::new(config);

        let candidate = SwitchCandidate {
            ssid: "Better".to_owned(),
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            signal_dbm: -50,
            radio: "radio0".to_owned(),
            channel: 36,
            encryption: EncryptionType::Psk2,
            is_tollgate: false,
        };

        assert!(!mgr.should_switch(-50, &candidate));
        assert!(!mgr.should_switch(-45, &candidate));
        assert!(mgr.should_switch(-63, &candidate));
    }

    #[test]
    fn test_should_switch_below_floor() {
        let config = UpstreamManagerConfig {
            signal_floor_dbm: -85,
            ..UpstreamManagerConfig::default()
        };
        let mgr = UpstreamManager::new(config);

        let weak_candidate = SwitchCandidate {
            ssid: "Weak".to_owned(),
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            signal_dbm: -84,
            radio: "radio0".to_owned(),
            channel: 36,
            encryption: EncryptionType::Psk2,
            is_tollgate: false,
        };

        assert!(mgr.should_switch(-86, &weak_candidate));
    }

    fn make_scan(ssid: &str, signal: i32, radio: &str) -> ScanResult {
        ScanResult {
            bssid: format!("AA:BB:CC:DD:EE:{:02X}", ssid.len()),
            ssid: ssid.to_owned(),
            signal_dbm: signal,
            encryption: EncryptionType::Psk2,
            channel: 36,
            radio: radio.to_owned(),
        }
    }

    #[test]
    fn test_find_candidates_normal_mode() {
        let config = UpstreamManagerConfig {
            reseller_mode: false,
            ..UpstreamManagerConfig::default()
        };
        let mgr = UpstreamManager::new(config);

        let networks = vec![
            make_scan("Net1", -50, "radio0"),
            make_scan("Net2", -70, "radio0"),
            make_scan("Net3", -60, "radio1"),
        ];

        let candidates = mgr.find_candidates(&networks, false);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].ssid, "Net1");
        assert_eq!(candidates[1].ssid, "Net3");
        assert_eq!(candidates[2].ssid, "Net2");
    }

    #[test]
    fn test_find_candidates_reseller_mode() {
        let config = UpstreamManagerConfig {
            reseller_mode: true,
            tollgate_ssid_prefix: "TollGate".to_owned(),
            ..UpstreamManagerConfig::default()
        };
        let mgr = UpstreamManager::new(config);

        let networks = vec![
            make_scan("OtherNet", -40, "radio0"),
            make_scan("TollGate-5G", -65, "radio0"),
            make_scan("TollGate-2G", -75, "radio1"),
        ];

        let candidates = mgr.find_candidates(&networks, false);
        assert_eq!(candidates.len(), 3);
        assert!(candidates[0].is_tollgate);
        assert!(candidates[1].is_tollgate);
        assert!(!candidates[2].is_tollgate);
        assert_eq!(candidates[0].ssid, "TollGate-5G");
        assert_eq!(candidates[1].ssid, "TollGate-2G");
        assert_eq!(candidates[2].ssid, "OtherNet");
    }

    #[test]
    fn test_find_candidates_blacklist_filtered() {
        let config = UpstreamManagerConfig::default();
        let mut mgr = UpstreamManager::new(config);
        mgr.blacklist.add("Blacklisted");

        let networks = vec![
            make_scan("Good", -50, "radio0"),
            make_scan("Blacklisted", -40, "radio0"),
        ];

        let candidates = mgr.find_candidates(&networks, false);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].ssid, "Good");
    }

    #[test]
    fn test_manager_state_transitions() {
        let config = UpstreamManagerConfig::default();
        let mut mgr = UpstreamManager::new(config);

        assert_eq!(mgr.state(), &ManagerState::Disconnected);

        mgr.state = ManagerState::Connected {
            iface: "wlan0".to_owned(),
            ssid: "TestNet".to_owned(),
            signal_dbm: -55,
            radio: "radio0".to_owned(),
        };
        assert!(matches!(mgr.state(), ManagerState::Connected { .. }));

        mgr.state = ManagerState::Switching {
            from_ssid: "TestNet".to_owned(),
            to_ssid: "Better".to_owned(),
        };
        assert!(matches!(mgr.state(), ManagerState::Switching { .. }));

        mgr.mark_disconnected();
        assert_eq!(mgr.state(), &ManagerState::Disconnected);
    }

    #[test]
    fn test_find_candidates_emergency_includes_weak() {
        let config = UpstreamManagerConfig {
            signal_floor_dbm: -85,
            ..UpstreamManagerConfig::default()
        };
        let mgr = UpstreamManager::new(config);

        let networks = vec![
            make_scan("Strong", -50, "radio0"),
            make_scan("WeakButOnly", -88, "radio0"),
        ];

        let normal = mgr.find_candidates(&networks, false);
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0].ssid, "Strong");

        let emergency = mgr.find_candidates(&networks, true);
        assert_eq!(emergency.len(), 2);
    }

    #[test]
    fn test_config_default_values() {
        let config = UpstreamManagerConfig::default();
        assert_eq!(config.scan_interval, Duration::from_secs(300));
        assert_eq!(config.fast_check_interval, Duration::from_secs(30));
        assert_eq!(config.hysteresis_db, 12);
        assert_eq!(config.signal_floor_dbm, -85);
        assert_eq!(config.blacklist_ttl, Duration::from_secs(3600));
        assert_eq!(config.max_consecutive_failures, 3);
        assert_eq!(config.cooldown_duration, Duration::from_secs(600));
        assert_eq!(config.startup_grace_period, Duration::from_secs(90));
        assert!(!config.reseller_mode);
    }

    #[test]
    fn test_vendor_element_score_tollgate_ssid() {
        let processor = VendorElementProcessor::new();
        let scan = ScanResult {
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            ssid: "TollGate-ABCD".to_owned(),
            signal_dbm: -60,
            encryption: EncryptionType::Psk2,
            channel: 36,
            radio: "radio0".to_owned(),
        };
        let (_, score) = processor.extract_and_score(&scan);
        assert_eq!(score, 40); // -60 + 100
    }

    #[test]
    fn test_vendor_element_score_regular_ssid() {
        let processor = VendorElementProcessor::new();
        let scan = ScanResult {
            bssid: "AA:BB:CC:DD:EE:FF".to_owned(),
            ssid: "MyNetwork".to_owned(),
            signal_dbm: -55,
            encryption: EncryptionType::Psk2,
            channel: 36,
            radio: "radio0".to_owned(),
        };
        let (_, score) = processor.extract_and_score(&scan);
        assert_eq!(score, -55);
    }

    #[test]
    fn test_vendor_element_score_negative_signal() {
        let processor = VendorElementProcessor::new();

        let weak_tollgate = ScanResult {
            bssid: "AA:BB:CC:DD:EE:01".to_owned(),
            ssid: "TollGate-Weak".to_owned(),
            signal_dbm: -90,
            encryption: EncryptionType::Psk2,
            channel: 1,
            radio: "radio0".to_owned(),
        };
        let (_, score) = processor.extract_and_score(&weak_tollgate);
        assert_eq!(score, 10); // -90 + 100

        let weak_regular = ScanResult {
            bssid: "AA:BB:CC:DD:EE:02".to_owned(),
            ssid: "WeakNet".to_owned(),
            signal_dbm: -90,
            encryption: EncryptionType::Psk2,
            channel: 1,
            radio: "radio0".to_owned(),
        };
        let (_, score) = processor.extract_and_score(&weak_regular);
        assert_eq!(score, -90);
    }

    #[test]
    fn test_set_get_vendor_elements_stubbed() {
        let processor = VendorElementProcessor::new();

        let mut elements = HashMap::new();
        elements.insert("test_key".to_owned(), "test_value".to_owned());

        assert!(processor.set_local_ap_vendor_elements(&elements).is_ok());

        let result = processor.get_local_ap_vendor_elements();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
