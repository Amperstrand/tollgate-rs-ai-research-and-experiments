//! TollGate Core — resource-agnostic payment protocol library.
//!
//! Implements the TollGate v2 wire protocol for autonomous, device-to-device
//! payment of metered resource delivery. Built on Cashu ecash and Spilman
//! payment channels.
//!
//! This crate is transport and resource agnostic. Consumers provide a
//! [`Wallet`] and [`ResourceAdapter`] to integrate with specific deployments.

pub mod access;
pub mod adapter;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod framing;
pub mod metering;
pub mod peer;
pub mod pricing;
pub mod protocol;
pub mod session;
pub mod types;
pub mod wallet;
