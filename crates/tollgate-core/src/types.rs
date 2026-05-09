//! Shared domain types for tollgate-core.
//!
//! These are internal domain types — they are NOT serialized directly to
//! CBOR. Wire types live in [`crate::protocol`].

use crate::protocol::Hash32;

/// Monetary amount in the smallest unit of the currency (e.g., millisatoshis).
///
/// Wraps u64. This is our own Amount type — we don't depend on the cashu
/// crate's Amount type yet. Will be reconciled with CDK in M2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(pub u64);

impl Amount {
    pub const ZERO: Amount = Amount(0);

    #[must_use]
    pub fn saturating_add(self, other: Amount) -> Amount {
        Amount(self.0.saturating_add(other.0))
    }

    #[must_use]
    pub fn saturating_sub(self, other: Amount) -> Amount {
        Amount(self.0.saturating_sub(other.0))
    }
}

impl std::ops::Add for Amount {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.checked_add(rhs.0).expect("Amount overflow"))
    }
}

impl std::ops::Sub for Amount {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.checked_sub(rhs.0).expect("Amount underflow"))
    }
}

/// State of a single Spilman payment channel within a peer session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    Proposed,
    Active { cumulative_balance: Amount },
    RollingOver {
        old_channel_id: Hash32,
        new_channel_id: Option<Hash32>,
    },
    Settling { final_balance: Amount },
    Closed,
}
