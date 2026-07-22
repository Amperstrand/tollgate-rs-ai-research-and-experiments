//! Balance/usage JSON builders for the Go v1 REST API compatibility layer.
//!
//! Go v1 returns:
//!   - Usage:  `{"remaining": 55000, "step_size": 60000, "metric": "milliseconds"}`
//!   - Balance: `{"remaining": 55000, "allotment": 50000, "step_size": 60000}`
//!
//! The Rust server's native format differs (scaled balances, different field
//! names). This module bridges the two (Issue #42, Fixes 1 & 2).

use serde::Serialize;

/// Configuration snapshot for v1-compatible endpoints. Carries the step size,
/// metric name, and the per-peer allotment.
#[derive(Debug, Clone)]
pub struct V1Config {
    /// Billing step size in milliseconds.
    pub step_size: u64,
    /// The resource metric ("milliseconds", "bytes", …).
    pub metric: String,
    /// The current remaining balance in the metered unit.
    pub remaining: u64,
}

impl V1Config {
    /// Build from the node's advertised pricing interval range and a remaining
    /// balance. `step_size` is taken from the max interval (Go v1's 60s step).
    pub fn from_pricing(
        min_interval_ms: u32,
        max_interval_ms: u32,
        remaining: u64,
        metric: &str,
    ) -> Self {
        // Go v1 step_size is the max interval (60000 ms = 60s).
        let _ = min_interval_ms; // min is informational; step uses max.
        Self {
            step_size: max_interval_ms as u64,
            metric: metric.to_string(),
            remaining,
        }
    }

    /// The allotment: how much was originally granted for this step. In Go v1
    /// this is the step's worth of credit. We compute it as the remaining
    /// rounded up to the nearest step_size, or just `remaining` when it fits
    /// within one step (the simple case the issue describes).
    pub fn allotment(&self) -> u64 {
        if self.step_size == 0 {
            return self.remaining;
        }
        // Allotment = step_size when remaining fits in one step, else the
        // remaining itself (the simple alias the issue requests).
        self.remaining.max(self.step_size)
    }
}

/// Usage JSON — the Go v1 format: `{"remaining", "step_size", "metric"}`.
#[derive(Debug, Serialize)]
pub struct UsageJson {
    pub remaining: u64,
    pub step_size: u64,
    pub metric: String,
}

impl UsageJson {
    pub fn from_config(config: &V1Config) -> Self {
        Self {
            remaining: config.remaining,
            step_size: config.step_size,
            metric: config.metric.clone(),
        }
    }
}

/// Balance JSON — the Go v1 format with the `allotment` field added
/// (Issue #42, Fix 2): `{"remaining", "allotment", "step_size"}`.
#[derive(Debug, Serialize)]
pub struct BalanceJson {
    pub remaining: u64,
    pub allotment: u64,
    pub step_size: u64,
}

/// Build the Go v1-compatible balance JSON from a config snapshot. Includes
/// both `remaining` and `allotment` (alias) so Go v1 clients that read either
/// field work (Issue #42, Fix 2).
pub fn get_balance_json(config: &V1Config) -> BalanceJson {
    BalanceJson {
        remaining: config.remaining,
        allotment: config.allotment(),
        step_size: config.step_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(remaining: u64, step_size: u64) -> V1Config {
        V1Config {
            step_size,
            metric: "milliseconds".to_string(),
            remaining,
        }
    }

    #[test]
    fn usage_json_has_go_v1_fields() {
        let cfg = config(55000, 60000);
        let json = UsageJson::from_config(&cfg);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains("\"remaining\":55000"), "{s}");
        assert!(s.contains("\"step_size\":60000"), "{s}");
        assert!(s.contains("\"metric\":\"milliseconds\""), "{s}");
    }

    #[test]
    fn balance_json_includes_allotment_field() {
        // Issue #42 Fix 2: balance must include "allotment".
        let cfg = config(55000, 60000);
        let json = get_balance_json(&cfg);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains("\"remaining\":55000"), "{s}");
        assert!(s.contains("\"allotment\""), "allotment field must be present: {s}");
        assert!(s.contains("\"step_size\":60000"), "{s}");
    }

    #[test]
    fn allotment_is_step_size_when_remaining_smaller() {
        // remaining=5000, step_size=60000 → allotment=60000 (the full step).
        let cfg = config(5000, 60000);
        assert_eq!(cfg.allotment(), 60000);
    }

    #[test]
    fn allotment_is_remaining_when_larger_than_step() {
        // remaining=80000, step_size=60000 → allotment=80000.
        let cfg = config(80000, 60000);
        assert_eq!(cfg.allotment(), 80000);
    }

    #[test]
    fn from_pricing_uses_max_interval_as_step_size() {
        let cfg = V1Config::from_pricing(5000, 60000, 30000, "milliseconds");
        assert_eq!(cfg.step_size, 60000);
        assert_eq!(cfg.remaining, 30000);
        assert_eq!(cfg.metric, "milliseconds");
    }
}
