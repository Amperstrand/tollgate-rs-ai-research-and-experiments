//! In-process conformance mock of the charger wire contract — the 132-line
//! Node simulator (`pecan/scripts/ev-device-sim.mjs`) as pure Rust, driven
//! by virtual time. Deliberately shares the real simulator's quirks:
//! `stop` is silent, `done` fires at window end, `start` is acked.

use tollgate_core::Millis;

/// Pure model of one simulated charger device.
#[derive(Clone, Debug)]
pub struct MockCharger {
    /// Acked grant window, in virtual ms: (started_at, ends_at).
    window: Option<(Millis, Millis)>,
}

impl MockCharger {
    pub const fn new() -> Self {
        Self { window: None }
    }

    /// `charger/{id}/start` `{"end": epoch_sec}` → ack reply payload.
    /// Returns `None` if a session is already running (the real sim
    /// refuses double starts).
    pub fn start(&mut self, now: Millis, end_epoch_sec: u64) -> Option<&'static str> {
        if self.window.is_some() {
            return None;
        }
        let ends_at = Millis(end_epoch_sec * 1000);
        self.window = Some((now, ends_at));
        Some("start-acked")
    }

    /// `charger/{id}/stop` `"OFF"` → kills the countdown. **No reply**,
    /// matching the wire contract.
    pub fn stop(&mut self) {
        self.window = None;
    }

    /// Pump virtual time: returns the `done` payload when the window
    /// expires this tick.
    pub fn pump(&mut self, now: Millis) -> Option<&'static str> {
        match self.window {
            Some((_, ends_at)) if now >= ends_at => {
                self.window = None;
                Some("countdown-finished")
            }
            _ => None,
        }
    }

    pub fn is_idle(&self) -> bool {
        self.window.is_none()
    }
}

impl Default for MockCharger {
    fn default() -> Self {
        Self::new()
    }
}
