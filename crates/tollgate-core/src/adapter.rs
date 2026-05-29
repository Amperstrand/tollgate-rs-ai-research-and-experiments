//! Resource adapter trait — abstracts resource delivery and metering.
//!
//! Implementations handle the actual resource delivery (e.g., IP forwarding,
//! FIPS peering, electricity switching) and metering.
//!
//! See `docs/design/core/tollgate-metering.md` for metering semantics.

use crate::access::AccessLevel;
use crate::error::AdapterError;
use crate::metering::PeerMetrics;

use std::future::Future;

/// Resource adapter trait — abstracts resource delivery and metering.
///
/// Implementations handle the actual resource delivery (e.g., IP forwarding,
/// FIPS peering, electricity switching) and metering.
///
/// See `docs/design/core/tollgate-metering.md` for metering semantics.
pub trait ResourceAdapter: Send + Sync {
    /// Set the access level for a peer.
    ///
    /// Called when access level changes (e.g., from None to Active after
    /// channel opens, or to Suspended after metering dispute).
    fn set_peer_access(
        &self,
        peer_id: &[u8],
        level: AccessLevel,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send;

    /// Get current metering counters for a peer.
    fn peer_metrics(
        &self,
        peer_id: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send;

    /// Subscribe to metering updates for a peer.
    ///
    /// Returns a stream of metric updates. The adapter should push updates
    /// at the configured metering interval.
    /// For now, this returns a boxed future that yields a single PeerMetrics.
    /// The actual stream abstraction will be added when we choose an async runtime.
    fn subscribe_meter(
        &self,
        peer_id: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send;
}
