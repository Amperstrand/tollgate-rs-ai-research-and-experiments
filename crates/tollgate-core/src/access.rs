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
    Active,
    /// Free access — price is zero (e.g., peer is whitelisted or operator chose free).
    ZeroPrice,
    /// Temporarily suspended — e.g., metering dispute, balance verification failure.
    Suspended,
}

impl AccessLevel {
    /// Returns true if the access level allows resource delivery.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Active | Self::ZeroPrice)
    }
}
