//! V1 Go-router compatibility layer.
//!
//! Feature-gated behind `v1-compat`. Provides HTTP endpoints that mirror the
//! Go v1 TollGate router API, translating v1 requests into Driver operations.

// Library modules ported from experimental-v1-archive. Many functions and
// types are not yet wired into the active code paths (handlers use only the
// adapter functions; client/session/usage modules are for future CLI wiring).
// Dead-code silencing is intentional until incremental wiring completes.
#![allow(dead_code)]

pub mod pricing;

pub mod mac_resolver;

pub mod nostr;

pub mod wallet;

pub mod merchant;

pub mod ln_quotes;

pub mod crowsnest;

pub mod adapter;

pub mod client;

pub mod usage_tracker;

pub mod session_manager;

pub mod handlers;

pub mod http_client;

pub mod recovery;

use std::sync::Arc;

pub fn build_v1_router(driver: crate::driver::Driver, config: Arc<merchant::V1ServerConfig>) -> axum::Router {
    handlers::build_router(driver, config)
}
