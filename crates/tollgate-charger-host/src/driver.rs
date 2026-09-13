//! The canonical driver: translates the charger wire contract into
//! DeliveryEvents and DeliveryActions into wire commands.

use tollgate_core::{
    Caps, DeliveryAction, DeliveryEngine, DeliveryEvent, DeliveryPhase, Grant, Lease, Millis,
    Progress, TerminalKind, TerminalReport,
};

/// Price vector with zero cost on every axis (free delivery).
pub const PRICELESS: PriceVector = PriceVector { per_second_millis: 0 };

/// The price vector used to synthesize cost accrual from elapsed time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PriceVector {
    /// Cost accrued per delivered second, in milli-units of money.
    pub per_second_millis: u64,
}

/// A decoded inbound charger message (topic tail + payload).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Inbound {
    /// `charger/{id}/ack` — grant accepted; delivery is live.
    GrantAck { at: Millis },
    /// `charger/{id}/done` — grant window exhausted (device terminal).
    Done { at: Millis, raw: Vec<u8> },
    /// `charger/{id}/aborted` — device/user stop-early (device terminal).
    Aborted { at: Millis, raw: Vec<u8> },
}

/// Commands the host shell must execute.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outbound {
    /// Publish `charger/{id}/start` with the lease deadline (epoch seconds).
    PublishStart { device: String, end_epoch_sec: u64 },
    /// Publish `charger/{id}/stop` = `"OFF"` (revoke).
    PublishStop { device: String },
    /// Burn this much of the deposit.
    Settle { nonce: u64, amount_millis: u64 },
    /// Return the un-burnt deposit as change.
    Refund { nonce: u64, amount_millis: u64 },
}

/// Why the driver stopped the device before a device terminal arrived;
/// the wire `stop` carries no acknowledgement, so the driver self-settles
/// at the journal on the next tick, reporting this reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PendingStop {
    CapSeconds,
    CapCost,
    Revoked,
}

/// Driver for one device session. All decisions live in the wrapped
/// [`DeliveryEngine`]; this type only translates.
#[derive(Clone, Debug)]
pub struct ChargerDriver {
    device: String,
    engine: DeliveryEngine,
    price: PriceVector,
    grant: Grant,
    /// Monotonic instant the grant was acknowledged (accrual baseline).
    acked_at: Option<Millis>,
    /// Last synthesized observation — the journal.
    last: Progress,
    pending_stop: Option<PendingStop>,
}

impl ChargerDriver {
    /// Arm a session; returns the driver and the initial wire command.
    pub fn open(
        device: String,
        grant: Grant,
        price: PriceVector,
        now: Millis,
    ) -> (Self, Vec<Outbound>) {
        let mut engine = DeliveryEngine::new();
        engine.handle(DeliveryEvent::GrantIssued { grant });
        let end_epoch_sec = now.0 / 1000 + lease_seconds_remaining(&grant, now);
        let driver = Self {
            device: device.clone(),
            engine,
            price,
            grant,
            acked_at: None,
            last: Progress {
                units_total: 0,
                seconds_total: 0,
                cost_millis_total: 0,
                at: now,
            },
            pending_stop: None,
        };
        (driver, vec![Outbound::PublishStart { device, end_epoch_sec }])
    }

    /// Crash recovery: rebuild from the journal.
    pub fn resume(device: String, grant: Grant, price: PriceVector, last: Progress) -> Self {
        Self {
            device,
            engine: DeliveryEngine::resume(grant, last),
            price,
            grant,
            acked_at: Some(Millis(last.at.0.saturating_sub(last.seconds_total * 1000))),
            last,
            pending_stop: None,
        }
    }

    /// The journal the host must persist for crash recovery.
    pub fn journal(&self) -> (Grant, Progress) {
        (self.grant, self.last)
    }

    /// Current engine phase (exposed for hosts and conformance suites).
    pub fn phase(&self) -> DeliveryPhase {
        self.engine.phase()
    }

    fn synthesize(&self, now: Millis) -> Progress {
        let Some(acked) = self.acked_at else {
            return Progress {
                units_total: 0,
                seconds_total: 0,
                cost_millis_total: 0,
                at: now,
            };
        };
        let seconds = now.since(acked) / 1000;
        Progress {
            units_total: 0,
            seconds_total: seconds,
            cost_millis_total: seconds.saturating_mul(self.price.per_second_millis),
            at: now,
        }
    }

    /// Handle an inbound charger message.
    pub fn handle_message(&mut self, inbound: Inbound) -> Vec<Outbound> {
        match inbound {
            Inbound::GrantAck { at } => {
                self.acked_at = Some(at);
                Vec::new()
            }
            Inbound::Done { at, raw } => {
                if self.pending_stop.is_none() {
                    self.last = self.synthesize(at);
                }
                let report = self.terminal(TerminalKind::CapSeconds, at, Some(raw));
                self.dispatch(DeliveryEvent::TerminalObserved { report })
            }
            Inbound::Aborted { at, raw } => {
                if self.pending_stop.is_none() {
                    self.last = self.synthesize(at);
                }
                let report = self.terminal(TerminalKind::Completed, at, Some(raw));
                self.dispatch(DeliveryEvent::TerminalObserved { report })
            }
        }
    }

    /// Timer tick: synthesize progress, enforce caps/lease, then settle any
    /// pending stop at the frozen journal (one tick of grace — the wire
    /// `stop` gets no reply).
    pub fn tick(&mut self, now: Millis) -> Vec<Outbound> {
        if self.engine.phase() == DeliveryPhase::Done {
            return Vec::new();
        }
        if let Some(reason) = self.pending_stop {
            let kind = match reason {
                PendingStop::CapSeconds => TerminalKind::CapSeconds,
                PendingStop::CapCost => TerminalKind::CapCost,
                PendingStop::Revoked => TerminalKind::Revoked,
            };
            let report = self.terminal(kind, self.last.at, None);
            self.pending_stop = None;
            return self.dispatch(DeliveryEvent::TerminalObserved { report });
        }
        self.last = self.synthesize(now);
        let mut out = self.dispatch(DeliveryEvent::ProgressObserved { progress: self.last });
        out.extend(self.dispatch(DeliveryEvent::Tick { now }));
        out
    }

    /// The trust-holder asked to revoke the grant.
    pub fn revoke(&mut self, now: Millis) -> Vec<Outbound> {
        self.pending_stop = Some(PendingStop::Revoked);
        self.dispatch(DeliveryEvent::RevokeRequested { at: now })
    }

    fn terminal(&self, kind: TerminalKind, at: Millis, receipt: Option<Vec<u8>>) -> TerminalReport {
        TerminalReport {
            kind,
            units_total: self.last.units_total,
            seconds_total: self.last.seconds_total,
            cost_millis_total: self.last.cost_millis_total,
            at,
            receipt,
        }
    }

    fn dispatch(&mut self, event: DeliveryEvent) -> Vec<Outbound> {
        self.engine
            .handle(event)
            .into_iter()
            .filter_map(|action| match action {
                DeliveryAction::StopDelivery { .. } => {
                    if self.pending_stop.is_none() {
                        let reason =
                            if self.grant.caps.max_cost_millis.is_some_and(|m| self.last.cost_millis_total >= m) {
                                PendingStop::CapCost
                            } else {
                                PendingStop::CapSeconds
                            };
                        self.pending_stop = Some(reason);
                    }
                    Some(Outbound::PublishStop { device: self.device.clone() })
                }
                DeliveryAction::SettleDelivery { nonce, amount_millis } => {
                    Some(Outbound::Settle { nonce, amount_millis })
                }
                DeliveryAction::RefundChange { nonce, amount_millis } => {
                    Some(Outbound::Refund { nonce, amount_millis })
                }
                DeliveryAction::Alarm { .. } => None,
            })
            .collect()
    }
}

fn lease_seconds_remaining(grant: &Grant, now: Millis) -> u64 {
    grant.lease.expires_at.0.saturating_sub(now.0) / 1000
}

/// Helper for building a time-and-money-capped grant.
pub const fn time_money_grant(
    nonce: u64,
    deposit_millis: u64,
    max_seconds: u64,
    max_cost_millis: u64,
    expires_at_ms: u64,
) -> Grant {
    Grant {
        nonce,
        deposit_millis,
        caps: Caps {
            max_units: None,
            max_seconds: Some(max_seconds),
            max_cost_millis: Some(max_cost_millis),
        },
        lease: Lease { expires_at: Millis(expires_at_ms) },
    }
}
