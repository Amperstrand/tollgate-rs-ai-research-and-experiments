//! Conformance suite: ChargerDriver ↔ MockCharger over the documented wire
//! contract, driven by virtual time. Scenarios mirror
//! docs/design/experiments/meter-delivery-modularity.md.

use tollgate_charger_host::{ChargerDriver, Inbound, MockCharger, Outbound, PriceVector, PRICELESS};
use tollgate_core::{DeliveryPhase, Grant, Millis};

const DEPOSIT: u64 = 12_000;

fn harness(grant: Grant, price: PriceVector) -> Harness {
    let now = Millis(1_000_000);
    let (driver, initial) = ChargerDriver::open("atomB".into(), grant, price, now);
    let mut h = Harness {
        driver,
        sim: MockCharger::new(),
        now,
        log: Vec::new(),
    };
    h.execute(initial);
    h
}

struct Harness {
    driver: ChargerDriver,
    sim: MockCharger,
    now: Millis,
    log: Vec<Outbound>,
}

impl Harness {
    fn execute(&mut self, out: Vec<Outbound>) {
        for cmd in out {
            match &cmd {
                Outbound::PublishStart { end_epoch_sec, .. } => {
                    let ack = self.sim.start(self.now, *end_epoch_sec);
                    assert_eq!(ack, Some("start-acked"), "sim must ack the grant");
                    let ack_out = self.driver.handle_message(Inbound::GrantAck { at: self.now });
                    assert!(ack_out.is_empty());
                }
                Outbound::PublishStop { .. } => self.sim.stop(), // silent on the wire
                Outbound::Settle { .. } | Outbound::Refund { .. } => {}
            }
            self.log.push(cmd);
        }
    }

    /// Advance virtual time until settlement; returns (settle, refund).
    fn run_to_settle(&mut self, step_ms: u64, max_steps: usize) -> (u64, u64) {
        for _ in 0..max_steps {
            self.now = Millis(self.now.0 + step_ms);
            if let Some(payload) = self.sim.pump(self.now) {
                let out = self.driver.handle_message(Inbound::Done {
                    at: self.now,
                    raw: payload.as_bytes().to_vec(),
                });
                self.execute(out);
            }
            let out = self.driver.tick(self.now);
            self.execute(out);
            let mut settle = (None, None);
            for cmd in &self.log {
                match cmd {
                    Outbound::Settle { amount_millis, .. } => settle.0 = Some(*amount_millis),
                    Outbound::Refund { amount_millis, .. } => settle.1 = Some(*amount_millis),
                    _ => {}
                }
            }
            if let (Some(s), Some(r)) = settle {
                assert_eq!(self.driver.phase(), DeliveryPhase::Done);
                return (s, r);
            }
        }
        panic!("no settlement after {max_steps} steps");
    }

    fn abort(&mut self) {
        let out = self.driver.handle_message(Inbound::Aborted {
            at: self.now,
            raw: b"button".to_vec(),
        });
        self.execute(out);
    }

    fn revoke(&mut self) {
        let out = self.driver.revoke(self.now);
        self.execute(out);
    }
}


fn tmg(max_seconds: u64, max_cost: u64, lease_ms: u64) -> Grant {
    tollgate_charger_host::time_money_grant(7, DEPOSIT, max_seconds, max_cost, 1_000_000 + lease_ms)
}

#[test]
fn s1_window_runs_to_done_settles_time_cost() {
    // 60s window, price 100 milli/s, caps above the window cost.
    let mut h = harness(tmg(600, DEPOSIT, 60_000), PriceVector { per_second_millis: 100 });
    let (settle, refund) = h.run_to_settle(1_000, 200);
    assert_eq!(settle, 6_000, "60s × 100 milli/s");
    assert_eq!(refund, DEPOSIT - 6_000);
}

#[test]
fn s2_seconds_cap_stops_midwindow_and_settles_at_stop_time() {
    // Cap 30s inside a 60s lease: engine stops at 30; the wire stop is
    // silent; settle must use the journal frozen at stop time.
    let mut h = harness(tmg(30, DEPOSIT, 60_000), PriceVector { per_second_millis: 100 });
    let (settle, refund) = h.run_to_settle(1_000, 200);
    assert_eq!(settle, 3_000, "30s × 100 milli/s, not the grace-tick reading");
    assert_eq!(refund, DEPOSIT - 3_000);
}

#[test]
fn s3_money_cap_clamps_settlement() {
    // Price high enough that the money cap hits before the time cap.
    let mut h = harness(tmg(600, 2_000, 60_000), PriceVector { per_second_millis: 100 });
    let (settle, refund) = h.run_to_settle(1_000, 200);
    assert_eq!(settle, 2_000, "clamped to max_cost");
    assert_eq!(refund, DEPOSIT - 2_000);
}

#[test]
fn s4_device_abort_settles_stop_early() {
    let mut h = harness(tmg(600, DEPOSIT, 60_000), PriceVector { per_second_millis: 100 });
    // Let 25 seconds elapse, then the button is pressed.
    for _ in 0..25 {
        h.now = Millis(h.now.0 + 1_000);
        let out = h.driver.tick(h.now);
        h.execute(out);
    }
    h.abort();
    let (settle, refund) = h.run_to_settle(1_000, 10);
    assert_eq!(settle, 2_500, "25s delivered before abort");
    assert_eq!(refund, DEPOSIT - 2_500);
}

#[test]
fn s5_revoke_settles_at_journal() {
    let mut h = harness(tmg(600, DEPOSIT, 60_000), PriceVector { per_second_millis: 100 });
    for _ in 0..10 {
        h.now = Millis(h.now.0 + 1_000);
        let out = h.driver.tick(h.now);
        h.execute(out);
    }
    h.revoke();
    let (settle, refund) = h.run_to_settle(1_000, 10);
    assert_eq!(settle, 1_000, "10s delivered before revoke");
    assert_eq!(refund, DEPOSIT - 1_000);
}

#[test]
fn s6_crash_resume_settles_identically() {
    let grant = tmg(600, DEPOSIT, 60_000);
    let price = PriceVector { per_second_millis: 100 };
    // Uninterrupted: 40s then abort.
    let mut a = harness(grant, price);
    for _ in 0..40 {
        a.now = Millis(a.now.0 + 1_000);
        let out = a.driver.tick(a.now);
        a.execute(out);
    }
    a.abort();
    let want = a.run_to_settle(1_000, 10);
    // Crashed at t=15s: journal = (grant, progress@15s). New driver resumes.
    let (g, last) = {
        let mut b = harness(grant, price);
        for _ in 0..15 {
            b.now = Millis(b.now.0 + 1_000);
            let out = b.driver.tick(b.now);
        b.execute(out);
        }
        b.driver.journal()
    };
    assert_eq!(last.seconds_total, 15);
    let mut c = Harness {
        driver: ChargerDriver::resume("atomB".into(), g, price, last),
        sim: MockCharger::new(),
        now: Millis(1_000_000 + 15_000),
        log: Vec::new(),
    };
    for _ in 0..25 {
        c.now = Millis(c.now.0 + 1_000);
        let out = c.driver.tick(c.now);
        c.execute(out);
    }
    c.abort();
    let got = c.run_to_settle(1_000, 10);
    assert_eq!(want, got, "settlement must be crash-invariant");
}

#[test]
fn s7_free_delivery_settles_zero_and_refunds_all() {
    let mut h = harness(tmg(30, DEPOSIT, 30_000), PRICELESS);
    let (settle, refund) = h.run_to_settle(1_000, 200);
    assert_eq!(settle, 0);
    assert_eq!(refund, DEPOSIT);
}
