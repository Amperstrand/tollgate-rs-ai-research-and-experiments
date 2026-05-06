//! TollGate Core — resource-agnostic payment protocol library.
//!
//! Implements the TollGate v2 wire protocol for autonomous, device-to-device
//! payment of metered resource delivery. Built on Cashu ecash and Spilman
//! payment channels.
//!
//! This crate is transport and resource agnostic. Consumers provide a
//! [`Wallet`] and [`ResourceAdapter`] to integrate with specific deployments.

pub mod protocol;

// Re-export primary protocol types at crate root for convenience.
pub use protocol::Message;
