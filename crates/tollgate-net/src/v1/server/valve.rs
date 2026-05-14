use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValveError {
    #[error("valve error: {0}")]
    Other(String),
}

pub trait Valve: Send + Sync {
    fn open_gate(&self, mac_address: &str) -> Result<(), ValveError>;
    fn close_gate(&self, mac_address: &str) -> Result<(), ValveError>;
}

/// Stub valve that logs gate operations at info level.
///
/// Does not actually gate traffic — real iptables/nftables integration is M4.
/// Use this for development and testing where payment handling is verified
/// without real traffic control.
pub struct StubValve;

impl Valve for StubValve {
    fn open_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::info!(
            mac = mac_address,
            "VALVE OPEN: traffic allowed (stub, no real gating)"
        );
        Ok(())
    }

    fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::info!(
            mac = mac_address,
            "VALVE CLOSE: session should end, traffic should be blocked (stub, no real gating)"
        );
        Ok(())
    }
}
