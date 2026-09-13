//! Conformance for the external-segment driver against a mock provider
//! shaped like the reverse-mapped municipal APIs (quote → session → poll →
//! receipt), proving the core DeliveryEngine works unchanged for
//! third-party meters.

use tollgate_charger_host::external::{ExternalAction, ExternalDriver, ExternalProviderPort, ProviderError, Receipt};
use tollgate_core::Millis;

struct MockMunicipalProvider {
    milli_per_second: u64,
    started_at: Option<Millis>,
    /// Receipt says LESS than the granted window (user stopped early).
    stop_early_seconds: Option<u64>,
    refuse: bool,
    sessions_started: u64,
}

impl MockMunicipalProvider {
    fn priced(milli_per_second: u64) -> Self {
        Self {
            milli_per_second,
            started_at: None,
            stop_early_seconds: None,
            refuse: false,
            sessions_started: 0,
        }
    }
}

impl ExternalProviderPort for MockMunicipalProvider {
    fn quote(&mut self, max_seconds: u64) -> u64 {
        max_seconds * self.milli_per_second
    }

    fn start(&mut self, _nonce: u64, _max_seconds: u64) -> Result<u64, ProviderError> {
        if self.refuse {
            return Err(ProviderError::Refused);
        }
        self.sessions_started += 1;
        Ok(9000 + self.sessions_started)
    }

    fn poll(&mut self, _session: u64, now: Millis) -> tollgate_core::Progress {
        let started = self.started_at.unwrap_or(now);
        self.started_at = Some(started);
        let seconds = now.since(started) / 1000;
        tollgate_core::Progress {
            units_total: 0,
            seconds_total: seconds,
            cost_millis_total: seconds * self.milli_per_second,
            at: now,
        }
    }

    fn stop(&mut self, _session: u64) -> Receipt {
        let seconds = self.stop_early_seconds.unwrap_or_else(|| {
            self.started_at.map(|s| s.0).map(|_| 60).unwrap_or(60)
        });
        Receipt {
            cost_millis_total: seconds * self.milli_per_second,
            seconds_total: seconds,
        }
    }
}

fn settle_refund(actions: &[ExternalAction]) -> (u64, u64) {
    let mut out = (0, 0);
    for a in actions {
        match a {
            ExternalAction::Settle { amount_millis, .. } => out.0 = *amount_millis,
            ExternalAction::Refund { amount_millis, .. } => out.1 = *amount_millis,
        }
    }
    out
}

#[test]
fn e1_full_window_settles_the_quote() {
    let mut port = MockMunicipalProvider::priced(100);
    let (mut d, _) = ExternalDriver::open(&mut port, 1, 60, Millis(0), 120_000).unwrap();
    let mut all = Vec::new();
    for t in (1..=60).step_by(10) {
        all.extend(d.tick(&mut port, Millis(t * 1000)));
    }
    port.stop_early_seconds = None;
    let mut stop_port = MockMunicipalProvider::priced(100);
    stop_port.started_at = port.started_at;
    stop_port.stop_early_seconds = Some(60);
    all.extend(d.stop(&mut stop_port, Millis(60_000)));
    let (settle, refund) = settle_refund(&all);
    assert_eq!(settle, 6_000, "60s x 100 milli — receipt matches quote");
    assert_eq!(refund, 0, "deposit was exactly the quote");
}

#[test]
fn e2_early_stop_receipt_is_truth_and_change_refunds() {
    let mut port = MockMunicipalProvider::priced(100);
    let (mut d, _) = ExternalDriver::open(&mut port, 2, 60, Millis(0), 120_000).unwrap();
    for t in (1..=3).step_by(1) {
        d.tick(&mut port, Millis(t * 1000));
    }
    // User stops after 25s: the RECEIPT (not our journal) is the truth.
    let mut stop_port = MockMunicipalProvider::priced(100);
    stop_port.stop_early_seconds = Some(25);
    stop_port.started_at = port.started_at;
    let actions = d.stop(&mut stop_port, Millis(25_000));
    let (settle, refund) = settle_refund(&actions);
    assert_eq!(settle, 2_500, "receipt says 25s delivered");
    assert_eq!(refund, 6_000 - 2_500, "deposit minus receipt refunds as change");
}

#[test]
fn e3_provider_refusal_blocks_the_grant_entirely() {
    let mut port = MockMunicipalProvider::priced(100);
    port.refuse = true;
    assert!(ExternalDriver::open(&mut port, 3, 60, Millis(0), 120_000).is_err());
    assert_eq!(port.sessions_started, 0, "no provider session on refusal");
}

#[test]
fn e4_free_window_costs_nothing_and_refunds_everything() {
    let mut port = MockMunicipalProvider::priced(0);
    let (mut d, _) = ExternalDriver::open(&mut port, 4, 30, Millis(0), 60_000).unwrap();
    for t in (1..=3) {
        d.tick(&mut port, Millis(t * 1000));
    }
    let mut stop_port = MockMunicipalProvider::priced(0);
    stop_port.stop_early_seconds = Some(30);
    stop_port.started_at = port.started_at;
    let actions = d.stop(&mut stop_port, Millis(30_000));
    assert_eq!(settle_refund(&actions), (0, 0), "free window: quote 0, nothing locked");
}

#[test]
fn e5_stalled_polls_lease_expire_settle_at_journal() {
    let mut port = MockMunicipalProvider::priced(100);
    let (mut d, _) = ExternalDriver::open(&mut port, 5, 600, Millis(0), 10_000).unwrap();
    // Polls anchor at the first call (t=2s): by t=11s the provider's own
    // accrual reads 9s. The lease (10s) lapses on that tick and the engine
    // settles at the last observed reading — the provider's number, not ours.
    d.tick(&mut port, Millis(2_000));
    d.tick(&mut port, Millis(4_000));
    let actions = d.tick(&mut port, Millis(11_000));
    let (settle, refund) = settle_refund(&actions);
    assert_eq!(settle, 900, "9s observed x 100");
    assert_eq!(refund, 60_000 - 900);
}

#[test]
fn e6_crash_resume_settles_identically() {
    let mut port = MockMunicipalProvider::priced(100);
    let (mut a, _) = ExternalDriver::open(&mut port, 6, 60, Millis(0), 120_000).unwrap();
    for t in 1..=20 {
        a.tick(&mut port, Millis(t * 1000));
    }
    let (grant, last) = a.journal();
    assert_eq!(last.seconds_total, 19, "journal at crash (first poll anchors t=1s)");
    drop(a); // crash
    // Host rebuilds from the journal; the same provider session keeps
    // accruing server-side (poll returns the provider's own numbers).
    let mut port2 = MockMunicipalProvider::priced(100);
    port2.started_at = port.started_at;
    let mut b = ExternalDriver::resume(grant, last, 9001);
    for t in 21..=40 {
        b.tick(&mut port2, Millis(t * 1000));
    }
    let mut stop_port = MockMunicipalProvider::priced(100);
    stop_port.stop_early_seconds = Some(40);
    stop_port.started_at = port2.started_at;
    let actions = b.stop(&mut stop_port, Millis(40_000));
    assert_eq!(settle_refund(&actions).0, 4_000, "40s x 100 — crash-invariant settle");
}

#[test]
fn e7_one_grant_maps_to_one_provider_session() {
    let mut port = MockMunicipalProvider::priced(100);
    let (mut d, _) = ExternalDriver::open(&mut port, 7, 60, Millis(0), 120_000).unwrap();
    for t in (1..=10).step_by(5) {
        d.tick(&mut port, Millis(t * 1000));
    }
    assert_eq!(port.sessions_started, 1, "ticks never re-start the session");
}
