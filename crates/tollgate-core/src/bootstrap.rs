//! Bootstrap token payment session logic.
//!
//! `BootstrapSession` tracks a single peer's bootstrap balance and processes
//! metering intervals. Balance is maintained at full scaled precision (i128)
//! to avoid per-interval rounding errors. Ceiling division to whole sats
//! only happens at display or settlement time.

use crate::access::AccessLevel;
use crate::metering::PeerMetrics;
use crate::pricing::compute_interval_cost_scaled;
use crate::types::Amount;

pub struct BootstrapSession {
    balance_scaled: i128,
    pricing_scale: u64,
    price_per_second: i64,
    price_per_unit: i64,
    last_metrics: PeerMetrics,
    access_level: AccessLevel,
}

pub enum BootstrapIntervalResult {
    Ok { balance_scaled: i128, cost_scaled: i128 },
    Exhausted { balance_scaled: i128, cost_scaled: i128 },
    CounterWentBackwards,
}

impl BootstrapSession {
    pub fn new(
        token_value: Amount,
        pricing_scale: u64,
        price_per_second: i64,
        price_per_unit: i64,
    ) -> Self {
        let balance_scaled =
            i128::from(token_value.0) * i128::from(pricing_scale);
        Self {
            balance_scaled,
            pricing_scale,
            price_per_second,
            price_per_unit,
            last_metrics: PeerMetrics::zero(),
            access_level: AccessLevel::Active,
        }
    }

    pub fn top_up(&mut self, token_value: Amount) {
        let add =
            i128::from(token_value.0) * i128::from(self.pricing_scale);
        self.balance_scaled += add;
        if self.access_level == AccessLevel::Suspended {
            self.access_level = AccessLevel::Active;
        }
    }

    pub fn process_interval(
        &mut self,
        current_metrics: &PeerMetrics,
    ) -> BootstrapIntervalResult {
        let Some(delta) = self.last_metrics.delta(current_metrics) else {
            return BootstrapIntervalResult::CounterWentBackwards;
        };

        let cost_scaled = compute_interval_cost_scaled(
            delta.elapsed_ms,
            delta.delivered,
            self.price_per_second,
            self.price_per_unit,
        );

        self.balance_scaled -= cost_scaled;
        self.last_metrics = current_metrics.clone();

        if self.balance_scaled <= 0 {
            self.access_level = AccessLevel::Suspended;
            BootstrapIntervalResult::Exhausted {
                balance_scaled: self.balance_scaled,
                cost_scaled,
            }
        } else {
            BootstrapIntervalResult::Ok {
                balance_scaled: self.balance_scaled,
                cost_scaled,
            }
        }
    }

    pub fn balance_scaled(&self) -> i128 {
        self.balance_scaled
    }

    pub fn is_exhausted(&self) -> bool {
        self.balance_scaled <= 0
    }

    pub fn access_level(&self) -> AccessLevel {
        self.access_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(sats: u64) -> BootstrapSession {
        BootstrapSession::new(Amount(sats), 1000, 10, 1)
    }

    fn metrics(elapsed_ms: u64, delivered: u64) -> PeerMetrics {
        PeerMetrics {
            elapsed_ms,
            delivered,
            received: 0,
        }
    }

    #[test]
    fn new_session_has_correct_balance() {
        let s = make_session(100);
        assert_eq!(s.balance_scaled(), 100_000);
        assert_eq!(s.access_level(), AccessLevel::Active);
        assert!(!s.is_exhausted());
    }

    #[test]
    fn process_interval_deducts_cost() {
        let mut s = make_session(100);
        let result = s.process_interval(&metrics(5000, 1000));
        match result {
            BootstrapIntervalResult::Ok { balance_scaled, cost_scaled } => {
                assert_eq!(cost_scaled, 1050);
                assert_eq!(balance_scaled, 100_000 - 1050);
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn top_up_adds_to_balance() {
        let mut s = make_session(100);
        s.process_interval(&metrics(5000, 1000));
        let before = s.balance_scaled();
        s.top_up(Amount(50));
        assert_eq!(s.balance_scaled(), before + 50_000);
    }

    #[test]
    fn top_up_resumes_from_suspended() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 10, 1);
        s.process_interval(&metrics(200_000, 0));
        assert_eq!(s.access_level(), AccessLevel::Suspended);
        s.top_up(Amount(100));
        assert_eq!(s.access_level(), AccessLevel::Active);
        assert!(s.balance_scaled() > 0);
    }

    #[test]
    fn multiple_intervals_monotonic_decrease() {
        let mut s = make_session(1000);
        let mut prev_balance = s.balance_scaled();
        for i in 1..=5u64 {
            let result = s.process_interval(&metrics(i * 5000, i * 100));
            match result {
                BootstrapIntervalResult::Ok { balance_scaled, .. } => {
                    assert!(balance_scaled < prev_balance);
                    prev_balance = balance_scaled;
                }
                _ => panic!("expected Ok at interval {i}"),
            }
        }
    }

    #[test]
    fn exhaustion_detected() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 10, 1);
        let result = s.process_interval(&metrics(200_000, 0));
        assert!(matches!(result, BootstrapIntervalResult::Exhausted { .. }));
    }

    #[test]
    fn exhaustion_suspends_access() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 10, 1);
        s.process_interval(&metrics(200_000, 0));
        assert_eq!(s.access_level(), AccessLevel::Suspended);
        assert!(s.is_exhausted());
    }

    #[test]
    fn top_up_after_exhaustion() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 10, 1);
        s.process_interval(&metrics(200_000, 0));
        assert!(s.is_exhausted());
        s.top_up(Amount(50));
        assert_eq!(s.access_level(), AccessLevel::Active);
        assert!(!s.is_exhausted());
    }

    #[test]
    fn zero_cost_interval() {
        let mut s = make_session(100);
        let before = s.balance_scaled();
        let result = s.process_interval(&metrics(0, 0));
        match result {
            BootstrapIntervalResult::Ok { balance_scaled, cost_scaled } => {
                assert_eq!(cost_scaled, 0);
                assert_eq!(balance_scaled, before);
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn counter_went_backwards() {
        let mut s = make_session(100);
        s.process_interval(&metrics(5000, 1000));
        let result = s.process_interval(&metrics(3000, 500));
        assert!(matches!(result, BootstrapIntervalResult::CounterWentBackwards));
    }

    #[test]
    fn large_token_value() {
        let s = BootstrapSession::new(Amount(10_000), 1000, 0, 0);
        assert_eq!(s.balance_scaled(), 10_000_000);
    }

    #[test]
    fn tiny_intervals_preserve_precision() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 0, 1);
        for i in 1..=100u64 {
            let result = s.process_interval(&metrics(i, i));
            match result {
                BootstrapIntervalResult::Ok { cost_scaled, .. } => {
                    assert_eq!(cost_scaled, 1);
                }
                BootstrapIntervalResult::Exhausted { .. } => break,
                BootstrapIntervalResult::CounterWentBackwards => {
                    panic!("unexpected backwards")
                }
            }
        }
    }

    #[test]
    fn negative_price_gives_credit() {
        let mut s = BootstrapSession::new(Amount(10), 1000, 0, -1);
        let before = s.balance_scaled();
        s.process_interval(&metrics(0, 100));
        assert!(s.balance_scaled() > before);
    }

    #[test]
    fn scaled_precision_preserved() {
        let mut s = BootstrapSession::new(Amount(1), 1000, 0, 1);
        let m1 = metrics(0, 1);
        s.process_interval(&m1);
        assert_eq!(s.balance_scaled(), 999);
        let m2 = metrics(0, 2);
        s.process_interval(&m2);
        assert_eq!(s.balance_scaled(), 998);
    }

    #[test]
    fn balance_scaled_reflects_exact_deduction() {
        let mut s = BootstrapSession::new(Amount(5), 1000, 3, 7);
        let cost = 5 * 3 + 200 * 7;
        let result = s.process_interval(&metrics(5000, 200));
        match result {
            BootstrapIntervalResult::Ok { balance_scaled, cost_scaled } => {
                assert_eq!(cost_scaled, cost);
                assert_eq!(balance_scaled, 5000 - cost);
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn repeated_top_ups_accumulate() {
        let mut s = make_session(10);
        s.top_up(Amount(5));
        s.top_up(Amount(3));
        assert_eq!(s.balance_scaled(), (10 + 5 + 3) * 1000);
    }
}
