//! Spilman payment channel module.
//!
//! Feature-gated behind `spilman`. Provides client-side Spilman channel
//! management and keyset utilities built on `cdk-spilman`.
//!
//! Ported from experimental-v1-archive. Not yet wired into the active
//! Driver/server code paths; dead-code silencing is intentional until
//! incremental wiring completes.

#![allow(dead_code)]

pub mod service;
pub mod wallet;
