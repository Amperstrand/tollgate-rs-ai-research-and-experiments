//! Access control types.
//!
//! Defines the access levels that govern resource delivery to peers.

/// Access level for a peer session.
///
/// Determines what network access the peer currently has.
/// See docs/design/core/tollgate-access-control.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessLevel {
    /// No access — peer is connected but not authorized.
    #[default]
    None,
    /// Full access — peer has an active paying channel.
    // RFC 8506: No direct equivalent. Implicit in RFC — client has quota and
    // can deliver.
    Active,
    /// Free access — price is zero (e.g., peer is whitelisted or operator chose free).
    ZeroPrice,
    /// Restricted access — throttled but not cut off (RFC 8506 RESTRICT_ACCESS).
    Restricted,
    /// Temporarily suspended — e.g., metering dispute, balance verification failure.
    // RFC 8506: Partial mapping to idle timeout / session termination state.
    Suspended,
}

impl AccessLevel {
    /// Returns true if the access level allows resource delivery.
    // RFC 8506: Provider-side access gating. RFC 8506's CCC gates service
    // delivery based on OCS quota; TollGate's provider gates based on local
    // balance tracking.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Active | Self::ZeroPrice | Self::Restricted)
    }
}
