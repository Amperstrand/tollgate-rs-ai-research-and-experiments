//! v1 compatibility layer — REST endpoints that match the Go v1 server's HTTP
//! API, so existing clients and monitoring tools built against `tollgate-module-basic-go`
//! keep working against this Rust server.
//!
//! Endpoints (served alongside the CBOR v2 protocol on the same port):
//!   GET  /v1/usage        — JSON {remaining, step_size, metric}
//!   GET  /v1/balance      — JSON {remaining, allotment, step_size}
//!   GET  /v1/whoami       — plain text (node identity)
//!   POST /v1/log-beacon   — diagnostic logging (Issue #69)
//!
//! The advertisement builder (`merchant`) emits both `step_size` and `step` tags
//! so Go v1 and Rust clients can parse it interchangeably (Issue #42, Fix 3).

pub mod adapter;
pub mod handlers;
pub mod merchant;
