//! Broker-parity tier: the same conformance scenarios as `conformance.rs`,
//! but against the REAL `ev-device-sim` (Node) over a REAL MQTT broker.
//! Proves the in-process mock speaks the same wire contract.
//!
//! Environment: `BROKER` (default `localhost:1883`). The sim must be
//! running for all devices (`DEVICES=atomA,atomB,atomC`).
//!
//! Run:
//! ```text
//! cargo test -p tollgate-charger-host --features real-broker --test broker_parity -- --nocapture
//! ```
//! Windows are real seconds (2–3s per scenario); the whole suite needs
//! about 20 seconds of wall time.

#![cfg(feature = "real-broker")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use tollgate_charger_host::{ChargerDriver, PriceVector, PRICELESS};
use tollgate_core::{Millis, Progress};

const DEPOSIT: u64 = 12_000;
const PRICE: PriceVector = PriceVector { per_second_millis: 100 };

fn now_ms() -> Millis {
    Millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    )
}

fn settle_of(log: &[String]) -> Option<(u64, u64)> {
    let mut s = None;
    let mut r = None;
    for l in log {
        if let Some(v) = l.strip_prefix("SETTLE ") {
            s = v.parse().ok();
        }
        if let Some(v) = l.strip_prefix("REFUND ") {
            r = v.parse().ok();
        }
    }
    Some((s?, r?))
}

/// One real-time session against the live broker + sim.
struct Real {
    client: AsyncClient,
    device: String,
    driver: ChargerDriver,
    log: Vec<String>,
    events: rumqttc::EventLoop,
}

async fn connect(id: &str) -> (AsyncClient, rumqttc::EventLoop) {
    let broker =
        std::env::var("BROKER").unwrap_or_else(|_| "localhost".into());
    let mut opts = MqttOptions::new(format!("parity-{id}"), broker, 1883);
    opts.set_keep_alive(Duration::from_secs(10));
    AsyncClient::new(opts, 20)
}

impl Real {
    async fn open(device: &str, max_seconds: u64, max_cost: u64, lease_s: u64, price: PriceVector) -> Self {
        let (client, mut events) = connect(device).await;
        client
            .subscribe(format!("charger/{device}/#"), QoS::AtLeastOnce)
            .await
            .unwrap();
        // Let the SUBACK land.
        while let Ok(ev) = events.poll().await {
            if let Event::Incoming(Incoming::SubAck(_)) = ev {
                break;
            }
        }
        let now = now_ms();
        let grant = tollgate_charger_host::time_money_grant(
            42,
            DEPOSIT,
            max_seconds,
            max_cost,
            now.0 + lease_s * 1000,
        );
        let (driver, initial) = ChargerDriver::open(device.into(), grant, price, now);
        let mut r = Self {
            client,
            device: device.into(),
            driver,
            log: Vec::new(),
            events,
        };
        r.execute(initial).await;
        r.wait_for_ack().await;
        r
    }

    /// Drain until the grant-ack lands, so accrual starts at the real ack
    /// instant instead of the first drain (which would undercount).
    async fn wait_for_ack(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while self.driver.acked_at().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no grant-ack from the sim — is ev-device-sim running?"
            );
            self.drain().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn execute(&mut self, out: Vec<tollgate_charger_host::Outbound>) {
        for cmd in out {
            use tollgate_charger_host::Outbound::*;
            match cmd.clone() {
                PublishStart { device, end_epoch_sec } => {
                    let payload = format!("{{\"end\":{end_epoch_sec}}}");
                    self.client
                        .publish(format!("charger/{device}/start"), QoS::AtLeastOnce, false, payload)
                        .await
                        .unwrap();
                }
                PublishStop { device } => {
                    self.client
                        .publish(format!("charger/{device}/stop"), QoS::AtLeastOnce, false, "OFF")
                        .await
                        .unwrap();
                }
                Settle { amount_millis, .. } => self.log.push(format!("SETTLE {amount_millis}")),
                Refund { amount_millis, .. } => self.log.push(format!("REFUND {amount_millis}")),
            }
        }
    }

    /// Drain ready inbound broker messages into the driver (non-blocking:
    /// returns after a short idle so the tick loop keeps running).
    async fn drain(&mut self) {
        loop {
            let ev = match tokio::time::timeout(Duration::from_millis(25), self.events.poll()).await
            {
                Ok(result) => result.expect("event loop error"),
                Err(_) => return,
            };
            let Event::Incoming(Incoming::Publish(p)) = ev else { continue };
            let at = now_ms();
            let topic = p.topic.clone();
            let payload = String::from_utf8_lossy(&p.payload).to_string();
            let out = if topic.ends_with("/ack") {
                self.driver.handle_message(tollgate_charger_host::Inbound::GrantAck { at })
            } else if topic.ends_with("/done") {
                self.driver.handle_message(tollgate_charger_host::Inbound::Done {
                    at,
                    raw: payload.into_bytes(),
                })
            } else if topic.ends_with("/aborted") {
                self.driver.handle_message(tollgate_charger_host::Inbound::Aborted {
                    at,
                    raw: payload.into_bytes(),
                })
            } else {
                Vec::new()
            };
            self.execute(out).await;
        }
    }

    /// Pump ticks + broker messages until settlement (or panic on timeout).
    async fn run_to_settle(&mut self, timeout: Duration) -> (u64, u64) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() > deadline {
                panic!("no settlement within {timeout:?} — log: {:?}", self.log);
            }
            self.drain().await;
            let out = self.driver.tick(now_ms());
            self.execute(out).await;
            if let Some(sr) = settle_of(&self.log) {
                return sr;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The e2e button path: the button-sim publishes `aborted` on the wire.
    async fn press_abort_button(&mut self) {
        self.client
            .publish(
                format!("charger/{}/aborted", self.device),
                QoS::AtLeastOnce,
                false,
                "button",
            )
            .await
            .unwrap();
    }
}

fn assert_window_settle(settle: u64, lo: u64, hi: u64) {
    assert!(
        (lo..=hi).contains(&settle),
        "settle {settle} outside expected window [{lo},{hi}] (ms of cost)"
    );
}

#[tokio::test]
async fn p1_full_window_done() {
    let mut r = Real::open("atomA", 30, DEPOSIT, 3, PRICE).await;
    // Lease 3s ⇒ wire end ≈ now+2..3s ⇒ settle ∈ {200, 300}.
    let (settle, refund) = r.run_to_settle(Duration::from_secs(10)).await;
    assert_window_settle(settle, 200, 300);
    assert_eq!(refund, DEPOSIT - settle);
}

#[tokio::test]
async fn p2_seconds_cap_midwindow() {
    let mut r = Real::open("atomB", 2, DEPOSIT, 10, PRICE).await;
    let (settle, refund) = r.run_to_settle(Duration::from_secs(10)).await;
    // Cap 2s; the wire stop is silent; settle from the frozen journal.
    assert_eq!(settle, 200, "2s × 100 milli/s at the stop-time journal");
    assert_eq!(refund, DEPOSIT - 200);
}

#[tokio::test]
async fn p3_money_cap_clamps() {
    let mut r = Real::open("atomC", 30, 300, 10, PRICE).await;
    let (settle, refund) = r.run_to_settle(Duration::from_secs(10)).await;
    assert_eq!(settle, 300, "clamped to max_cost at the stop-time journal");
    assert_eq!(refund, DEPOSIT - 300);
}

#[tokio::test]
async fn p4_button_abort_settles_stop_early() {
    let mut r = Real::open("atomA", 30, DEPOSIT, 30, PRICE).await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let out = r.driver.tick(now_ms());
    r.execute(out).await;
    r.press_abort_button().await;
    let (settle, refund) = r.run_to_settle(Duration::from_secs(5)).await;
    assert_window_settle(settle, 200, 300, );
    assert_eq!(refund, DEPOSIT - settle);
}

#[tokio::test]
async fn p5_revoke_settles_at_journal() {
    let mut r = Real::open("atomB", 30, DEPOSIT, 30, PRICE).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let out = r.driver.tick(now_ms());
    r.execute(out).await;
    let out = r.driver.revoke(now_ms());
    r.execute(out).await;
    let (settle, refund) = r.run_to_settle(Duration::from_secs(5)).await;
    assert_window_settle(settle, 100, 200);
    assert_eq!(refund, DEPOSIT - settle);
}

#[tokio::test]
async fn p6_crash_resume_against_live_countdown() {
    // Driver A crashes after ~1.2s. Driver B resumes from the journal; the
    // sim's own countdown keeps running and delivers `done`.
    let mut a = Real::open("atomC", 30, DEPOSIT, 4, PRICE).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    a.drain().await;
    let out = a.driver.tick(now_ms());
    a.execute(out).await;
    let (grant, last): (_, Progress) = a.driver.journal();
    assert_eq!(last.seconds_total, 1, "journal at crash time");
    drop(a); // crash

    let (client, events) = connect("p6b").await;
    client
        .subscribe("charger/atomC/#", QoS::AtLeastOnce)
        .await
        .unwrap();
    let mut b = Real {
        client,
        device: "atomC".into(),
        driver: ChargerDriver::resume("atomC".into(), grant, PRICE, last),
        log: Vec::new(),
        events,
    };
    let (settle, refund) = b.run_to_settle(Duration::from_secs(10)).await;
    assert_window_settle(settle, 300, 400, );
    assert_eq!(refund, DEPOSIT - settle);
}

#[tokio::test]
async fn p7_free_delivery_settles_zero() {
    let mut r = Real::open("atomA", 3, DEPOSIT, 3, PRICELESS).await;
    let (settle, refund) = r.run_to_settle(Duration::from_secs(10)).await;
    assert_eq!(settle, 0);
    assert_eq!(refund, DEPOSIT);
}
