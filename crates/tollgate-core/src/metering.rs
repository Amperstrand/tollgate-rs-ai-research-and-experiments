/// Cumulative metering counters for a peer.
///
/// These are monotonically increasing counters (never reset).
/// See docs/design/core/tollgate-metering.md for the cumulative counter model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerMetrics {
    /// Total milliseconds elapsed since session start.
    pub elapsed_ms: u64,
    /// Total units delivered (e.g., bytes forwarded).
    pub delivered: u64,
    /// Total units received (e.g., bytes received from peer).
    pub received: u64,
}

impl PeerMetrics {
    pub fn zero() -> Self {
        Self { elapsed_ms: 0, delivered: 0, received: 0 }
    }

    /// Compute the delta between two metric snapshots.
    /// Returns None if `newer` counters are less than `self` (would indicate reset).
    pub fn delta(&self, newer: &PeerMetrics) -> Option<MetricDelta> {
        if newer.elapsed_ms < self.elapsed_ms
            || newer.delivered < self.delivered
            || newer.received < self.received
        {
            return None;
        }
        Some(MetricDelta {
            elapsed_ms: newer.elapsed_ms - self.elapsed_ms,
            delivered: newer.delivered - self.delivered,
            received: newer.received - self.received,
        })
    }
}

/// Delta between two metric snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDelta {
    pub elapsed_ms: u64,
    pub delivered: u64,
    pub received: u64,
}

/// Result of a metering calibration check.
///
/// Metering calibration compares the peer's reported metrics against
/// our own observations. If transit loss exceeds the configured threshold,
/// the session may be suspended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalibrationResult {
    /// Metrics are within acceptable bounds.
    WithinTolerance,
    /// Transit loss exceeds threshold.
    TransitLossExceeded {
        expected: u64,
        observed: u64,
        threshold_pct: u8,
    },
}

/// Transit loss threshold configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitLossThreshold {
    /// Maximum allowed percentage difference between delivered and received.
    pub max_loss_pct: u8,
    /// Minimum absolute difference before checking percentage (to avoid false positives on small numbers).
    pub min_absolute: u64,
}
