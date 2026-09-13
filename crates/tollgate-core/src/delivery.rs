//! Budgeted delivery authorizations: grants, reports, and the meter guard.
//!
//! This module implements the Meter/Delivery split from
//! `docs/design/experiments/meter-delivery-modularity.md` and
//! `non-fungible-minutes.md` in the house style: a pure sans-IO state
//! machine. The **trust-holding host** (the Meter module) translates real
//! I/O — a charger's meter ticks, a provider's session poll, a device's
//! stop confirmation — into [`DeliveryEvent`]s, and executes the
//! [`DeliveryAction`]s this machine emits.
//!
//! The contract in one paragraph: a [`Grant`] is a scoped capability
//! (`{nonce, caps, lease}`) authorizing a price-blind delivery agent to
//! deliver up to three caps — units, seconds, and money — whichever hits
//! first. Delivery is observed through cumulative [`Progress`] reports
//! (timestamped, so tariffs are integrable after the fact) and ends with a
//! [`TerminalReport`] whose receipt is the settlement truth. Settlement
//! burns exactly what the receipt says and refunds the un-burnt deposit;
//! a grant that is never exercised lapses with its lease, settling at the
//! last journalled reading.

use alloc::vec;
use alloc::vec::Vec;

use crate::time::Millis;

/// Opaque settlement evidence carried by a terminal report (the provider's
/// final response, or our own signed ledger entry).
pub type Receipt = Vec<u8>;

/// The three caps of a budget. `None` means unbounded on that dimension;
/// at least one bound must exist for a grant to be issuable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caps {
    /// Maximum cumulative units (watt-hours, bytes, …).
    pub max_units: Option<u64>,
    /// Maximum cumulative delivery seconds.
    pub max_seconds: Option<u64>,
    /// Maximum cumulative cost in milli-units of money. First-class because
    /// tariff granularity makes raw units non-fungible
    /// (`non-fungible-minutes.md`).
    pub max_cost_millis: Option<u64>,
}

impl Caps {
    /// Whether any bound is set at all.
    pub const fn bounded(&self) -> bool {
        self.max_units.is_some() || self.max_seconds.is_some() || self.max_cost_millis.is_some()
    }
}

/// Expiry-if-not-exercised terms. The lease is what makes crash recovery
/// self-healing: no terminal report before `expires_at` → settle at the
/// last journalled reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lease {
    /// Absolute monotonic deadline for the whole grant.
    pub expires_at: Millis,
}

/// The scoped authorization issued by the trust-holder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grant {
    /// Unique id enabling idempotency (one grant ↔ one external session).
    pub nonce: u64,
    /// Deposit locked up-front, in milli-units of money.
    pub deposit_millis: u64,
    pub caps: Caps,
    pub lease: Lease,
}

/// A cumulative, timestamped accrual observation. Totals are monotonic;
/// the timestamp is what lets any tariff be integrated after the fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Progress {
    pub units_total: u64,
    pub seconds_total: u64,
    pub cost_millis_total: u64,
    /// Host-stamped observation time.
    pub at: Millis,
}

/// Why delivery ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalKind {
    /// Stopped before any cap; user or provider initiated.
    Completed,
    /// Units cap reached.
    CapUnits,
    /// Time cap reached.
    CapSeconds,
    /// Money cap reached (settle clamps to the cap).
    CapCost,
    /// The delivery agent failed before or during delivery.
    Failed,
    /// The trust-holder revoked the grant.
    Revoked,
    /// No terminal report before the lease deadline; settled at journal.
    LeaseExpired,
}

/// The final outcome. `cost_millis_total` is receipt-attested where the
/// provider supplies one; `receipt` is the evidence payload.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TerminalReport {
    pub kind: TerminalKind,
    pub units_total: u64,
    pub seconds_total: u64,
    pub cost_millis_total: u64,
    pub at: Millis,
    pub receipt: Option<Receipt>,
}

/// Inputs to the delivery state machine, produced by the host from real I/O.
#[derive(Clone, Debug)]
pub enum DeliveryEvent {
    /// The trust-holder issued a grant (deposit already locked).
    GrantIssued { grant: Grant },
    /// A cumulative meter observation arrived (device tick or provider poll).
    ProgressObserved { progress: Progress },
    /// The delivery agent's final report arrived.
    TerminalObserved { report: TerminalReport },
    /// The trust-holder asked to revoke.
    RevokeRequested { at: Millis },
    /// Periodic timer tick with the host's monotonic now.
    Tick { now: Millis },
}

/// Effects the core asks the host to carry out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeliveryAction {
    /// Tell the delivery agent to stop (cap reached or revoke requested).
    StopDelivery { nonce: u64 },
    /// Burn exactly this much of the deposit; the receipt is the evidence.
    SettleDelivery { nonce: u64, amount_millis: u64 },
    /// Return the un-burnt deposit as change.
    RefundChange { nonce: u64, amount_millis: u64 },
    /// Anomaly the host should log/raise (e.g. non-monotonic meter).
    Alarm { nonce: u64, reason: AlarmReason },
}

/// Why the machine raised an [`DeliveryAction::Alarm`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlarmReason {
    /// A progress sample went backwards; the sample was ignored and the
    /// last journalled reading stands.
    NonMonotonicProgress,
    /// A terminal report arrived with no active grant.
    UnmatchedTerminal,
}

/// Phase of a single-delivery engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeliveryPhase {
    /// No grant issued yet.
    Idle,
    /// Grant issued; waiting for first progress or terminal.
    Granted,
    /// Progress observed; caps being enforced.
    Delivering,
    /// Terminal processed; settlement emitted. Terminal state.
    Done,
}

/// Pure budget guard: decides, from a grant and the latest cumulative
/// progress, whether delivery may continue. Kept separate from the engine
/// so hosts (and tests) can use it directly against polled readings.
#[derive(Clone, Copy, Debug)]
pub struct BudgetGuard {
    grant: Grant,
    last: Progress,
}

impl BudgetGuard {
    pub const fn new(grant: Grant) -> Self {
        Self {
            grant,
            last: Progress {
                units_total: 0,
                seconds_total: 0,
                cost_millis_total: 0,
                at: Millis(0),
            },
        }
    }

    /// The journalled reading this guard would settle on if the grant
    /// lapsed right now.
    pub const fn journalled(&self) -> Progress {
        self.last
    }

    /// Apply a new cumulative observation. Returns `Err(())` when the
    /// sample is non-monotonic (ignored; journal stands), otherwise the
    /// cap that is now exhausted, if any.
    pub fn apply(&mut self, progress: Progress) -> Result<Option<TerminalKind>, ()> {
        if progress.units_total < self.last.units_total
            || progress.seconds_total < self.last.seconds_total
            || progress.cost_millis_total < self.last.cost_millis_total
        {
            return Err(());
        }
        self.last = progress;
        Ok(self.exhausted())
    }

    /// Which caps are exhausted at the current reading.
    pub fn exhausted(&self) -> Option<TerminalKind> {
        if let Some(max) = self.grant.caps.max_units {
            if self.last.units_total >= max {
                return Some(TerminalKind::CapUnits);
            }
        }
        if let Some(max) = self.grant.caps.max_seconds {
            if self.last.seconds_total >= max {
                return Some(TerminalKind::CapSeconds);
            }
        }
        if let Some(max) = self.grant.caps.max_cost_millis {
            if self.last.cost_millis_total >= max {
                return Some(TerminalKind::CapCost);
            }
        }
        None
    }
}

/// Single-delivery state machine (one active grant per engine). Hosts run
/// one engine per delivery; multi-delivery bookkeeping is host state.
#[derive(Clone, Debug)]
pub struct DeliveryEngine {
    phase: DeliveryPhase,
    guard: Option<BudgetGuard>,
    issued_at: Option<Millis>,
}

impl DeliveryEngine {
    pub const fn new() -> Self {
        Self { phase: DeliveryPhase::Idle, guard: None, issued_at: None }
    }

    /// Crash recovery: rebuild from the journal (the grant and the last
    /// progress the host managed to persist).
    pub fn resume(grant: Grant, last: Progress) -> Self {
        let mut guard = BudgetGuard::new(grant);
        // Tolerate the journal itself via best-effort apply.
        let _ = guard.apply(last);
        Self {
            phase: if guard.exhausted().is_some() {
                DeliveryPhase::Granted
            } else {
                DeliveryPhase::Delivering
            },
            guard: Some(guard),
            issued_at: Some(last.at),
        }
    }

    pub const fn phase(&self) -> DeliveryPhase {
        self.phase
    }

    /// Advance the machine. Returns the actions the host must execute.
    pub fn handle(&mut self, event: DeliveryEvent) -> Vec<DeliveryAction> {
        match event {
            DeliveryEvent::GrantIssued { grant } => match self.phase {
                DeliveryPhase::Idle => {
                    debug_assert!(
                        grant.caps.bounded(),
                        "a grant must carry at least one cap"
                    );
                    self.guard = Some(BudgetGuard::new(grant));
                    self.phase = DeliveryPhase::Granted;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            DeliveryEvent::ProgressObserved { progress } => match &mut self.guard {
                Some(guard) => match guard.apply(progress) {
                    Err(()) => vec![DeliveryAction::Alarm {
                        nonce: self.nonce(),
                        reason: AlarmReason::NonMonotonicProgress,
                    }],
                    Ok(Some(_kind)) => {
                        self.phase = DeliveryPhase::Delivering;
                        vec![DeliveryAction::StopDelivery { nonce: self.nonce() }]
                    }
                    Ok(None) => {
                        self.phase = DeliveryPhase::Delivering;
                        Vec::new()
                    }
                },
                None => Vec::new(),
            },
            DeliveryEvent::TerminalObserved { report } => match self.phase {
                DeliveryPhase::Idle => vec![DeliveryAction::Alarm {
                    nonce: report.kind as u64,
                    reason: AlarmReason::UnmatchedTerminal,
                }],
                DeliveryPhase::Granted | DeliveryPhase::Delivering => {
                    self.settle_from_totals(
                        report.cost_millis_total,
                        report.kind,
                    )
                }
                DeliveryPhase::Done => Vec::new(),
            },
            DeliveryEvent::RevokeRequested { .. } => match self.phase {
                DeliveryPhase::Granted | DeliveryPhase::Delivering => {
                    vec![DeliveryAction::StopDelivery { nonce: self.nonce() }]
                }
                _ => Vec::new(),
            },
            DeliveryEvent::Tick { now } => match (&self.guard, self.phase) {
                (Some(guard), DeliveryPhase::Granted | DeliveryPhase::Delivering) => {
                    if now >= guard.grant.lease.expires_at {
                        let journalled = guard.journalled();
                        self.settle_from_totals(journalled.cost_millis_total, TerminalKind::LeaseExpired)
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            },
        }
    }

    /// Compute settlement from an attested final cost (or the journal) and
    /// close the engine. Settle clamps to the deposit and to the money cap;
    /// the remainder refunds.
    fn settle_from_totals(
        &mut self,
        final_cost_millis: u64,
        _kind: TerminalKind,
    ) -> Vec<DeliveryAction> {
        let Some(guard) = self.guard else {
            return Vec::new();
        };
        let grant = guard.grant;
        let settle = final_cost_millis
            .min(grant.deposit_millis)
            .min(grant.caps.max_cost_millis.unwrap_or(u64::MAX));
        let refund = grant.deposit_millis - settle;
        self.phase = DeliveryPhase::Done;
        vec![
            DeliveryAction::SettleDelivery { nonce: grant.nonce, amount_millis: settle },
            DeliveryAction::RefundChange { nonce: grant.nonce, amount_millis: refund },
        ]
    }

    const fn nonce(&self) -> u64 {
        match &self.guard {
            Some(g) => g.grant.nonce,
            None => 0,
        }
    }
}

impl Default for DeliveryEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPOSIT: u64 = 12_000;

    fn grant(caps: Caps, lease_ms: u64) -> Grant {
        Grant {
            nonce: 7,
            deposit_millis: DEPOSIT,
            caps,
            lease: Lease { expires_at: Millis(lease_ms) },
        }
    }

    fn progress(units: u64, seconds: u64, cost: u64, at: u64) -> Progress {
        Progress { units_total: units, seconds_total: seconds, cost_millis_total: cost, at: Millis(at) }
    }

    fn settle_refund(actions: &[DeliveryAction]) -> (u64, u64) {
        let mut out = (0, 0);
        for a in actions {
            match a {
                DeliveryAction::SettleDelivery { amount_millis, .. } => out.0 = *amount_millis,
                DeliveryAction::RefundChange { amount_millis, .. } => out.1 = *amount_millis,
                _ => {}
            }
        }
        out
    }

    #[test]
    fn happy_path_settles_receipt_and_refunds_remainder() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued {
            grant: grant(Caps { max_units: Some(100), max_seconds: Some(600), max_cost_millis: Some(DEPOSIT) }, 100_000),
        });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 60, 1000, 1_000) });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(20, 120, 2000, 2_000) });
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport {
                kind: TerminalKind::Completed,
                units_total: 20,
                seconds_total: 120,
                cost_millis_total: 2500,
                at: Millis(2_500),
                receipt: Some(b"stop-receipt".into()),
            },
        });
        assert_eq!(settle_refund(&actions), (2500, DEPOSIT - 2500));
        assert_eq!(e.phase(), DeliveryPhase::Done);
    }

    #[test]
    fn units_cap_forces_stop_then_settles_at_cap() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued {
            grant: grant(Caps { max_units: Some(10), max_seconds: None, max_cost_millis: None }, 100_000),
        });
        let actions = e.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 5, 500, 100) });
        assert!(actions.contains(&DeliveryAction::StopDelivery { nonce: 7 }));
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport {
                kind: TerminalKind::CapUnits,
                units_total: 10,
                seconds_total: 5,
                cost_millis_total: 500,
                at: Millis(150),
                receipt: None,
            },
        });
        assert_eq!(settle_refund(&actions), (500, DEPOSIT - 500));
    }

    #[test]
    fn seconds_cap_forces_stop() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued {
            grant: grant(Caps { max_units: None, max_seconds: Some(60), max_cost_millis: None }, 100_000),
        });
        let actions = e.handle(DeliveryEvent::ProgressObserved { progress: progress(5, 60, 700, 1_000) });
        assert!(actions.contains(&DeliveryAction::StopDelivery { nonce: 7 }));
    }

    #[test]
    fn money_cap_clamps_settlement() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued {
            grant: grant(Caps { max_units: None, max_seconds: None, max_cost_millis: Some(5_000) }, 100_000),
        });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(1, 1, 4_000, 100) });
        // Terminal claims MORE than the cap; settlement clamps.
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::CapCost, units_total: 2, seconds_total: 2, cost_millis_total: 9_000, at: Millis(200), receipt: None },
        });
        assert_eq!(settle_refund(&actions), (5_000, DEPOSIT - 5_000));
    }

    #[test]
    fn failure_before_delivery_refunds_everything() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued { grant: grant(Caps { max_units: Some(10), max_seconds: None, max_cost_millis: None }, 100_000) });
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::Failed, units_total: 0, seconds_total: 0, cost_millis_total: 0, at: Millis(50), receipt: None },
        });
        assert_eq!(settle_refund(&actions), (0, DEPOSIT));
    }

    #[test]
    fn revoke_stops_and_refunds_remainder() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued { grant: grant(Caps { max_units: Some(100), max_seconds: None, max_cost_millis: None }, 100_000) });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 10, 3_000, 1_000) });
        let actions = e.handle(DeliveryEvent::RevokeRequested { at: Millis(1_100) });
        assert_eq!(actions, vec![DeliveryAction::StopDelivery { nonce: 7 }]);
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::Revoked, units_total: 10, seconds_total: 12, cost_millis_total: 3_200, at: Millis(1_150), receipt: None },
        });
        assert_eq!(settle_refund(&actions), (3_200, DEPOSIT - 3_200));
    }

    #[test]
    fn lease_expiry_settles_at_journalled_reading() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued { grant: grant(Caps { max_units: Some(100), max_seconds: None, max_cost_millis: None }, 10_000) });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 100, 4_000, 5_000) });
        // Silence... then the lease deadline passes.
        let actions = e.handle(DeliveryEvent::Tick { now: Millis(10_000) });
        assert_eq!(settle_refund(&actions), (4_000, DEPOSIT - 4_000));
        assert_eq!(e.phase(), DeliveryPhase::Done);
    }

    #[test]
    fn crash_resume_settles_identically_to_uninterrupted_run() {
        let g = grant(Caps { max_units: Some(100), max_seconds: None, max_cost_millis: None }, 100_000);
        // Uninterrupted run.
        let mut a = DeliveryEngine::new();
        a.handle(DeliveryEvent::GrantIssued { grant: g });
        a.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 100, 1_000, 1_000) });
        a.handle(DeliveryEvent::ProgressObserved { progress: progress(20, 200, 2_000, 2_000) });
        let want = a.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::Completed, units_total: 20, seconds_total: 200, cost_millis_total: 2_000, at: Millis(2_100), receipt: None },
        });
        // Crashed run: journal = (grant, last persisted progress at t=1000).
        let mut b = DeliveryEngine::resume(g, progress(10, 100, 1_000, 1_000));
        b.handle(DeliveryEvent::ProgressObserved { progress: progress(20, 200, 2_000, 2_000) });
        let got = b.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::Completed, units_total: 20, seconds_total: 200, cost_millis_total: 2_000, at: Millis(2_100), receipt: None },
        });
        assert_eq!(settle_refund(&want), settle_refund(&got));
    }

    #[test]
    fn non_monotonic_sample_alarms_and_journal_stands() {
        let mut e = DeliveryEngine::new();
        e.handle(DeliveryEvent::GrantIssued { grant: grant(Caps { max_units: Some(100), max_seconds: None, max_cost_millis: None }, 100_000) });
        e.handle(DeliveryEvent::ProgressObserved { progress: progress(10, 100, 1_000, 1_000) });
        let actions = e.handle(DeliveryEvent::ProgressObserved { progress: progress(5, 50, 500, 1_500) });
        assert!(actions.contains(&DeliveryAction::Alarm { nonce: 7, reason: AlarmReason::NonMonotonicProgress }));
        // Lease lapse settles on the journal (10 units / 1000 cost), not the bogus rewind.
        let actions = e.handle(DeliveryEvent::Tick { now: Millis(100_000) });
        assert_eq!(settle_refund(&actions), (1_000, DEPOSIT - 1_000));
    }

    #[test]
    fn unmatched_terminal_alarms() {
        let mut e = DeliveryEngine::new();
        let actions = e.handle(DeliveryEvent::TerminalObserved {
            report: TerminalReport { kind: TerminalKind::Completed, units_total: 0, seconds_total: 0, cost_millis_total: 0, at: Millis(1), receipt: None },
        });
        assert!(matches!(actions[0], DeliveryAction::Alarm { .. }));
    }
}
