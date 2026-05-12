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

pub struct StubValve;

impl Valve for StubValve {
    fn open_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::debug!("StubValve: opening gate for {mac_address}");
        Ok(())
    }

    fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
        tracing::debug!("StubValve: closing gate for {mac_address}");
        Ok(())
    }
}
