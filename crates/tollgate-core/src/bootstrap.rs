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

/// Action to take when buyer's quota is exhausted.
/// RFC 8506 §8.34 — Final-Unit-Action equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionAction {
    /// Hard cutoff — suspend access immediately when remaining_quota <= 0.
    /// RFC 8506 Final-Unit-Action TERMINATE (0).
    Terminate,
    /// Throttle bandwidth — continue delivery at reduced rate.
    /// RFC 8506 Final-Unit-Action RESTRICT_ACCESS (2).
    Restrict,
    /// Allow overdelivery — continue delivery beyond paid amount up to leeway.
    /// No RFC 8506 equivalent — TollGate-specific.
    Allow,
}

/// Configuration for quota exhaustion behavior.
#[derive(Debug, Clone, Copy)]
pub struct ExhaustionConfig {
    /// Action to take when quota is exhausted.
    pub action: ExhaustionAction,
    /// Allow mode: deliver N% extra beyond paid amount (0 = disabled).
    pub leeway_percent: u32,
    /// Allow mode: deliver N extra scaled units beyond zero (0 = disabled).
    pub leeway_units_scaled: i128,
}

impl Default for ExhaustionConfig {
    fn default() -> Self {
        Self {
            action: ExhaustionAction::Terminate,
            leeway_percent: 0,
            leeway_units_scaled: 0,
        }
    }
}

// RFC 8506: No direct equivalent. RFC's Credit-Control-Server tracks quota per session;
// TollGate tracks balance per peer. Inverted flow: buyer pays → provider credits
// (RFC: server grants → client delivers).
pub struct BootstrapSession {
    balance_scaled: i128,
    pricing_scale: u64,
    price_per_second: i64,
    price_per_unit: i64,
    last_metrics: PeerMetrics,
    access_level: AccessLevel,
    min_checkin_ms: u64,
    max_interval_ms: u64,
    exhaustion_config: ExhaustionConfig,
    effective_leeway_scaled: i128,
}

pub enum BootstrapIntervalResult {
    Ok {
        balance_scaled: i128,
        cost_scaled: i128,
        next_checkin_ms: u64,
        is_final: bool,
    },
    Exhausted {
        balance_scaled: i128,
        cost_scaled: i128,
        action: ExhaustionAction,
    },
    CounterWentBackwards,
}

impl BootstrapSession {
    pub fn new(
        token_value: Amount,
        pricing_scale: u64,
        price_per_second: i64,
        price_per_unit: i64,
        min_checkin_ms: u64,
        max_interval_ms: u64,
        exhaustion_config: ExhaustionConfig,
    ) -> Self {
        let balance_scaled = i128::from(token_value.0) * i128::from(pricing_scale);
        let initial_balance_scaled = balance_scaled;

        let leeway_percent_scaled =
            (initial_balance_scaled * i128::from(exhaustion_config.leeway_percent)) / 100;
        let effective_leeway_scaled =
            i128::max(exhaustion_config.leeway_units_scaled, leeway_percent_scaled);

        Self {
            balance_scaled,
            pricing_scale,
            price_per_second,
            price_per_unit,
            last_metrics: PeerMetrics::zero(),
            access_level: AccessLevel::Active,
            min_checkin_ms,
            max_interval_ms,
            exhaustion_config,
            effective_leeway_scaled,
        }
    }

    // RFC 8506: Partial mapping to server-initiated re-authorization (§5.5).
    pub fn top_up(&mut self, token_value: Amount) {
        let add = i128::from(token_value.0) * i128::from(self.pricing_scale);
        self.balance_scaled += add;
        if self.access_level == AccessLevel::Suspended {
            self.access_level = AccessLevel::Active;
        }
    }

    #[allow(clippy::too_many_lines)]
    // RFC 8506: Partial mapping to credit-control UPDATE_REQUEST handling (§5.2).
    // TollGate uses peer metrics delta instead of explicit quota request.
    pub fn process_interval(&mut self, current_metrics: &PeerMetrics) -> BootstrapIntervalResult {
        let Some(delta) = self.last_metrics.delta(current_metrics) else {
            return BootstrapIntervalResult::CounterWentBackwards;
        };

        let cost_scaled = compute_interval_cost_scaled(
            delta.elapsed_ms,
            delta.delivered,
            self.price_per_second,
            self.price_per_unit,
        );

        self.last_metrics = current_metrics.clone();

        let action = self.exhaustion_config.action;

        if cost_scaled > 0 && self.balance_scaled <= cost_scaled {
            match action {
                ExhaustionAction::Terminate => {
                    self.balance_scaled = 0;
                    self.access_level = AccessLevel::Suspended;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: 0,
                        cost_scaled,
                        action,
                    };
                }
                ExhaustionAction::Restrict => {
                    self.balance_scaled = 0;
                    self.access_level = AccessLevel::Restricted;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: 0,
                        cost_scaled,
                        action,
                    };
                }
                ExhaustionAction::Allow => {
                    // Allow balance to go negative
                    self.balance_scaled -= cost_scaled;
                    if self.effective_leeway_scaled > 0
                        && -self.balance_scaled <= self.effective_leeway_scaled
                    {
                        // Within leeway — continue with is_final: true
                        return BootstrapIntervalResult::Ok {
                            balance_scaled: self.balance_scaled,
                            cost_scaled,
                            next_checkin_ms: self.min_checkin_ms,
                            is_final: true,
                        };
                    }
                    // Leeway exhausted
                    self.access_level = AccessLevel::Suspended;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: self.balance_scaled,
                        cost_scaled,
                        action,
                    };
                }
            }
        }

        self.balance_scaled -= cost_scaled;

        if self.balance_scaled <= 0 {
            match action {
                ExhaustionAction::Terminate => {
                    self.balance_scaled = 0;
                    self.access_level = AccessLevel::Suspended;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: 0,
                        cost_scaled,
                        action,
                    };
                }
                ExhaustionAction::Restrict => {
                    self.balance_scaled = 0;
                    self.access_level = AccessLevel::Restricted;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: 0,
                        cost_scaled,
                        action,
                    };
                }
                ExhaustionAction::Allow => {
                    if self.effective_leeway_scaled > 0
                        && -self.balance_scaled <= self.effective_leeway_scaled
                    {
                        return BootstrapIntervalResult::Ok {
                            balance_scaled: self.balance_scaled,
                            cost_scaled,
                            next_checkin_ms: self.min_checkin_ms,
                            is_final: true,
                        };
                    }
                    self.access_level = AccessLevel::Suspended;
                    return BootstrapIntervalResult::Exhausted {
                        balance_scaled: self.balance_scaled,
                        cost_scaled,
                        action,
                    };
                }
            }
        }

        let next_checkin_ms = self.compute_next_checkin_ms(delta.elapsed_ms, delta.delivered);
        let is_final = self.compute_is_final(delta.elapsed_ms, delta.delivered);

        BootstrapIntervalResult::Ok {
            balance_scaled: self.balance_scaled,
            cost_scaled,
            next_checkin_ms,
            is_final,
        }
    }

    // RFC 8506: TollGate-specific adaptive Validity-Time (§8.33).
    // RFC implementations use fixed values; TollGate computes from remaining_quota / spend_rate.
    fn compute_next_checkin_ms(&self, last_elapsed_ms: u64, last_delivered: u64) -> u64 {
        if self.balance_scaled <= 0 {
            return self.min_checkin_ms;
        }

        let last_cost_scaled = compute_interval_cost_scaled(
            last_elapsed_ms,
            last_delivered,
            self.price_per_second,
            self.price_per_unit,
        );

        if last_elapsed_ms == 0 || last_cost_scaled <= 0 {
            return self.max_interval_ms;
        }

        let ms_remaining =
            (self.balance_scaled * i128::from(last_elapsed_ms)) / last_cost_scaled;

        let checkin = u64::try_from(ms_remaining).unwrap_or(u64::MAX);
        checkin.clamp(self.min_checkin_ms, self.max_interval_ms)
    }

    // RFC 8506: Direct mapping to Final-Unit-Indication (§8.34).
    fn compute_is_final(&self, last_elapsed_ms: u64, last_delivered: u64) -> bool {
        if self.balance_scaled <= 0 {
            return true;
        }

        let next_cost_scaled = compute_interval_cost_scaled(
            last_elapsed_ms,
            last_delivered,
            self.price_per_second,
            self.price_per_unit,
        );

        self.balance_scaled <= next_cost_scaled
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
        BootstrapSession::new(
            Amount(sats),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        )
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
            BootstrapIntervalResult::Ok {
                balance_scaled,
                cost_scaled,
                ..
            } => {
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
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
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
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let result = s.process_interval(&metrics(200_000, 0));
        assert!(matches!(result, BootstrapIntervalResult::Exhausted { .. }));
    }

    #[test]
    fn exhaustion_suspends_access() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        s.process_interval(&metrics(200_000, 0));
        assert_eq!(s.access_level(), AccessLevel::Suspended);
        assert!(s.is_exhausted());
    }

    #[test]
    fn top_up_after_exhaustion() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
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
            BootstrapIntervalResult::Ok {
                balance_scaled,
                cost_scaled,
                ..
            } => {
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
        assert!(matches!(
            result,
            BootstrapIntervalResult::CounterWentBackwards
        ));
    }

    #[test]
    fn large_token_value() {
        let s = BootstrapSession::new(
            Amount(10_000),
            1000,
            0,
            0,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        assert_eq!(s.balance_scaled(), 10_000_000);
    }

    #[test]
    fn tiny_intervals_preserve_precision() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            0,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
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
        let mut s = BootstrapSession::new(
            Amount(10),
            1000,
            0,
            -1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let before = s.balance_scaled();
        s.process_interval(&metrics(0, 100));
        assert!(s.balance_scaled() > before);
    }

    #[test]
    fn scaled_precision_preserved() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            0,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let m1 = metrics(0, 1);
        s.process_interval(&m1);
        assert_eq!(s.balance_scaled(), 999);
        let m2 = metrics(0, 2);
        s.process_interval(&m2);
        assert_eq!(s.balance_scaled(), 998);
    }

    #[test]
    fn balance_scaled_reflects_exact_deduction() {
        let mut s = BootstrapSession::new(
            Amount(5),
            1000,
            3,
            7,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let cost = 5 * 3 + 200 * 7;
        let result = s.process_interval(&metrics(5000, 200));
        match result {
            BootstrapIntervalResult::Ok {
                balance_scaled,
                cost_scaled,
                ..
            } => {
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

    #[test]
    fn exhaustion_balance_is_zero_not_negative() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let result = s.process_interval(&metrics(200_000, 0));
        match result {
            BootstrapIntervalResult::Exhausted { balance_scaled, .. } => {
                assert_eq!(balance_scaled, 0, "exhausted balance must be 0, not negative");
            }
            _ => panic!("expected Exhausted"),
        }
    }

    #[test]
    fn balance_never_goes_negative() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        s.process_interval(&metrics(200_000, 0));
        assert_eq!(s.balance_scaled(), 0);
    }

    #[test]
    fn next_checkin_ms_decreases_as_balance_decreases() {
        let mut s = make_session(100);
        let mut prev_checkin = u64::MAX;
        for i in 1..=5u64 {
            let result = s.process_interval(&metrics(i * 1000, i * 100));
            if let BootstrapIntervalResult::Ok { next_checkin_ms, .. } = result {
                assert!(
                    next_checkin_ms <= prev_checkin,
                    "checkin should decrease or stay same: {next_checkin_ms} vs {prev_checkin} at interval {i}"
                );
                prev_checkin = next_checkin_ms;
            }
        }
    }

    #[test]
    fn is_final_true_when_balance_low() {
        let mut s = BootstrapSession::new(
            Amount(2),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig::default(),
        );
        let result = s.process_interval(&metrics(5000, 1000));
        match result {
            BootstrapIntervalResult::Ok { is_final, .. } => {
                assert!(
                    is_final,
                    "should be is_final when next interval would exhaust"
                );
            }
            _ => panic!("expected Ok"),
        }
    }

    // ─── Phase 2: Exhaustion action tests ───

    #[test]
    fn terminate_action_suspends_access() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Terminate,
                leeway_percent: 0,
                leeway_units_scaled: 0,
            },
        );
        let result = s.process_interval(&metrics(200_000, 0));
        match result {
            BootstrapIntervalResult::Exhausted {
                balance_scaled,
                action,
                ..
            } => {
                assert_eq!(action, ExhaustionAction::Terminate);
                assert_eq!(balance_scaled, 0);
            }
            _ => panic!("expected Exhausted"),
        }
        assert_eq!(s.access_level(), AccessLevel::Suspended);
    }

    #[test]
    fn allow_action_continues_past_zero() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Allow,
                leeway_percent: 0,
                leeway_units_scaled: 5000, // 5 extra scaled units
            },
        );
        // cost_scaled = 200_000 * 10 = 2_000_000 >> balance (1000)
        let result = s.process_interval(&metrics(200_000, 0));
        match result {
            BootstrapIntervalResult::Ok {
                balance_scaled,
                is_final,
                ..
            } => {
                assert!(balance_scaled < 0, "balance should be negative with Allow");
                assert!(is_final, "should be is_final when past zero");
            }
            _ => panic!("expected Ok (within leeway)"),
        }
    }

    #[test]
    fn allow_action_exhausts_at_leeway() {
        // 1 sat with scale 1000 = 1000 scaled balance
        // price_per_second = 10, so 1 second costs 10_000 scaled
        // With leeway_units_scaled = 50, allow up to -50
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Allow,
                leeway_percent: 0,
                leeway_units_scaled: 50,
            },
        );
        // cost = 200_000 * 10 = 2_000_000 >> balance + leeway (1000 + 50 = 1050)
        let result = s.process_interval(&metrics(200_000, 0));
        match result {
            BootstrapIntervalResult::Exhausted {
                balance_scaled,
                action,
                ..
            } => {
                assert_eq!(action, ExhaustionAction::Allow);
                assert!(
                    -balance_scaled > 50,
                    "balance should be past leeway limit"
                );
            }
            _ => panic!("expected Exhausted (past leeway)"),
        }
        assert_eq!(s.access_level(), AccessLevel::Suspended);
    }

    #[test]
    fn allow_action_leeway_percent() {
        // 1 sat with scale 1000 = 1000 scaled balance
        // leeway_percent = 50 → 500 extra scaled units
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Allow,
                leeway_percent: 50,
                leeway_units_scaled: 0,
            },
        );
        // cost = 200_000 * 10 = 2_000_000 >> balance + leeway (1000 + 500 = 1500)
        let result = s.process_interval(&metrics(200_000, 0));
        assert!(
            matches!(result, BootstrapIntervalResult::Exhausted { .. }),
            "should exhaust past leeway"
        );
    }

    #[test]
    fn restrict_action_sets_restricted_access() {
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            10,
            1,
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Restrict,
                leeway_percent: 0,
                leeway_units_scaled: 0,
            },
        );
        let result = s.process_interval(&metrics(200_000, 0));
        match result {
            BootstrapIntervalResult::Exhausted {
                action,
                balance_scaled,
                ..
            } => {
                assert_eq!(action, ExhaustionAction::Restrict);
                assert_eq!(balance_scaled, 0);
            }
            _ => panic!("expected Exhausted"),
        }
        assert_eq!(s.access_level(), AccessLevel::Restricted);
    }

    #[test]
    fn allow_within_leeway_multiple_intervals() {
        // Small balance with leeway that covers a few intervals
        let mut s = BootstrapSession::new(
            Amount(1),
            1000,
            0,
            1, // 1 per unit
            1000,
            10000,
            ExhaustionConfig {
                action: ExhaustionAction::Allow,
                leeway_percent: 0,
                leeway_units_scaled: 500, // 500 extra scaled units beyond zero
            },
        );
        // balance = 1000, leeway = 500, so we can go to -500

        // Interval 1: deliver 500 units → cost = 500, balance = 500
        let r1 = s.process_interval(&metrics(0, 500));
        assert!(matches!(r1, BootstrapIntervalResult::Ok { .. }));

        // Interval 2: deliver 600 units → cost = 600, balance = -100 (within leeway)
        let r2 = s.process_interval(&metrics(0, 1100));
        match r2 {
            BootstrapIntervalResult::Ok {
                balance_scaled,
                is_final,
                ..
            } => {
                assert_eq!(balance_scaled, -100);
                assert!(is_final);
            }
            _ => panic!("expected Ok (within leeway)"),
        }

        // Interval 3: deliver 800 units → cost = 800, balance = -900 (past leeway -500)
        let r3 = s.process_interval(&metrics(0, 1900));
        match r3 {
            BootstrapIntervalResult::Exhausted { action, .. } => {
                assert_eq!(action, ExhaustionAction::Allow);
            }
            _ => panic!("expected Exhausted (past leeway)"),
        }
        assert_eq!(s.access_level(), AccessLevel::Suspended);
    }
}
