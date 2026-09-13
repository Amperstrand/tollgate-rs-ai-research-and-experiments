//! `tollgate-charger-host` — Meter-module driver for the charger MQTT
//! contract.
//!
//! Pure message translation, no I/O: [`ChargerDriver`] consumes decoded
//! inbound charger messages ([`Inbound`]) and timer ticks, delegates all
//! decisions to [`tollgate_core::DeliveryEngine`], and emits [`Outbound`]
//! commands (publishes plus settlement intents) for the host shell to
//! execute against a real broker. The wire contract and its canonical
//! mapping live in `docs/design/experiments/charger-mqtt-contract.md`.
//!
//! The device meters nothing (the countdown is the meter), so the driver
//! synthesizes cumulative [`Progress`](tollgate_core::Progress) from the
//! acknowledged start time and a price vector; caps on units and money are
//! therefore enforced here, on the trust-holding side — the device only
//! honors its time lease.

mod driver;
mod sim;

pub use driver::{time_money_grant, ChargerDriver, Inbound, Outbound, PriceVector, PRICELESS};
pub use sim::MockCharger;
