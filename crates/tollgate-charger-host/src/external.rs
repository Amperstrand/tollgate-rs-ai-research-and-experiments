//! External-segment driver: the Delivery contract for third-party meters.
//!
//! Where [`crate::ChargerDriver`] talks to a device we control, this module
//! models the other half of the ecosystem — a provider API we can only
//! *call* (municipal parking, commercial charging): the meter belongs to
//! someone else, we observe by polling, and the provider's stop response is
//! the settlement truth. This is the poll-as-tick / receipt-as-truth /
//! quote-before-commit shape documented in
//! `docs/design/experiments/meter-delivery-modularity.md` §"External
//! provider integration", extracted from two real reverse-mapped providers
//! (see the glossary's provider-adapter column).
//!
//! The trust-holding host implements [`ExternalProviderPort`] for its
//! provider; [`ExternalDriver`] does everything else on top of
//! [`tollgate_core::DeliveryEngine`]: sizes the deposit from the quote,
//! maps one grant nonce to at most one provider session (idempotency),
//! synthesizes PROGRESS from polls, and settles from the receipt.

use tollgate_core::{
    DeliveryAction, DeliveryEngine, DeliveryEvent, DeliveryPhase, Grant, Millis,
    Progress, TerminalKind, TerminalReport,
};

/// What a third-party meter adapter must provide. `start` is expected to be
/// idempotent per nonce (providers issue their own session ids; the driver
/// remembers the mapping).
pub trait ExternalProviderPort {
    /// Exact price for a capped window, in milli-units (quote-before-commit;
    /// the deposit estimator — a zero quote means a free window).
    fn quote(&mut self, max_seconds: u64) -> u64;
    /// Begin delivery. `nonce` is the idempotency key.
    fn start(&mut self, nonce: u64, max_seconds: u64) -> Result<u64, ProviderError>;
    /// Cumulative accrual at `now` (poll-as-tick).
    fn poll(&mut self, session: u64, now: Millis) -> Progress;
    /// Stop delivery; the returned receipt is the settlement truth.
    fn stop(&mut self, session: u64) -> Receipt;
}

/// Provider-side failure before any delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProviderError {
    Refused,
}

/// The provider's final answer — what actually happened, their accounting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Receipt {
    pub cost_millis_total: u64,
    pub seconds_total: u64,
}

/// Effects for the host: settle/refund plus, unlike the device contract,
/// a provider session handle the host may need for its own bookkeeping.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExternalAction {
    Settle { nonce: u64, amount_millis: u64 },
    Refund { nonce: u64, amount_millis: u64 },
}

/// One grant ↔ at most one provider session.
#[derive(Clone, Debug)]
pub struct ExternalDriver {
    engine: DeliveryEngine,
    grant: Grant,
    session: Option<u64>,
    last: Progress,
}

impl ExternalDriver {
    /// Quote the window, lock the deposit, start the provider session.
    pub fn open(
        port: &mut dyn ExternalProviderPort,
        nonce: u64,
        max_seconds: u64,
        now: Millis,
        lease_ms: u64,
    ) -> Result<(Self, Vec<ExternalAction>), ProviderError> {
        let deposit = port.quote(max_seconds);
        let session = port.start(nonce, max_seconds)?;
        let grant = Grant {
            nonce,
            deposit_millis: deposit,
            caps: tollgate_core::Caps {
                max_units: None,
                max_seconds: Some(max_seconds),
                max_cost_millis: None,
            },
            lease: tollgate_core::Lease { expires_at: Millis(now.0 + lease_ms) },
        };
        let mut engine = DeliveryEngine::new();
        engine.handle(DeliveryEvent::GrantIssued { grant });
        Ok((
            Self {
                engine,
                grant,
                session: Some(session),
                last: Progress {
                    units_total: 0,
                    seconds_total: 0,
                    cost_millis_total: 0,
                    at: now,
                },
            },
            Vec::new(),
        ))
    }

    pub fn journal(&self) -> (Grant, Progress) {
        (self.grant, self.last)
    }

    /// Crash recovery: rebuild from the journal plus the live provider
    /// session id (the host persisted the nonce↔session mapping).
    pub fn resume(grant: Grant, last: Progress, session: u64) -> Self {
        Self {
            engine: DeliveryEngine::resume(grant, last),
            grant,
            session: Some(session),
            last,
        }
    }

    pub fn phase(&self) -> DeliveryPhase {
        self.engine.phase()
    }

    /// Poll the provider, feed the engine, translate actions.
    pub fn tick(&mut self, port: &mut dyn ExternalProviderPort, now: Millis) -> Vec<ExternalAction> {
        if self.engine.phase() == DeliveryPhase::Done {
            return Vec::new();
        }
        if let Some(session) = self.session {
            self.last = port.poll(session, now);
        }
        let progress = self.last;
        let actions: Vec<DeliveryAction> = self.engine.handle(DeliveryEvent::ProgressObserved { progress });
        let mut out = self.translate(actions);
        let actions: Vec<DeliveryAction> = self.engine.handle(DeliveryEvent::Tick { now });
        out.extend(self.translate(actions));
        out
    }

    /// Stop delivery at the provider and settle from the receipt.
    pub fn stop(&mut self, port: &mut dyn ExternalProviderPort, now: Millis) -> Vec<ExternalAction> {
        if self.engine.phase() == DeliveryPhase::Done {
            return Vec::new();
        }
        let receipt = self.session.map(|s| port.stop(s));
        self.session = None;
        let (cost, seconds) = receipt
            .map(|r| (r.cost_millis_total, r.seconds_total))
            .unwrap_or((self.last.cost_millis_total, self.last.seconds_total));
        let report = TerminalReport {
            kind: TerminalKind::Completed,
            units_total: 0,
            seconds_total: seconds,
            cost_millis_total: cost,
            at: now,
            receipt: None,
        };
        let actions = self.engine.handle(DeliveryEvent::TerminalObserved { report });
        self.translate(actions)
    }

    fn translate(&mut self, actions: Vec<DeliveryAction>) -> Vec<ExternalAction> {
        actions
            .into_iter()
            .filter_map(|a| match a {
                DeliveryAction::SettleDelivery { nonce, amount_millis } => {
                    Some(ExternalAction::Settle { nonce, amount_millis })
                }
                DeliveryAction::RefundChange { nonce, amount_millis } => {
                    Some(ExternalAction::Refund { nonce, amount_millis })
                }
                _ => None,
            })
            .collect()
    }
}
