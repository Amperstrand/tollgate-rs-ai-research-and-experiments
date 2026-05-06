//! Peer session state machine and peer info.
//!
//! Defines the primary state machine for the protocol lifecycle.
//! Transitions are driven by incoming/outgoing messages.
//!
//! See `docs/design/core/tollgate-protocol.md` for the full state diagram.

use crate::access::AccessLevel;
use crate::metering::PeerMetrics;
use crate::protocol::{Hash32, PubKey};
use crate::types::{Amount, ChannelState};

/// State of a peer session (the main state machine).
///
/// This is the primary state machine for the protocol lifecycle.
/// Transitions are driven by incoming/outgoing messages.
/// See `docs/design/core/tollgate-protocol.md` for the full state diagram.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PeerSessionState {
    /// Initial state — connection established but no Announce exchanged.
    #[default]
    Initial,
    /// Announce received/sent, waiting for price negotiation.
    Announced,
    /// Price sheet accepted, channel funding in progress.
    Priced,
    /// Channel is active, metering and balance updates flowing.
    ChannelReady,
    /// Session is being torn down.
    Closing,
    /// Session is fully closed.
    Closed,
}

/// Information about a connected peer.
///
/// Holds all session state for a single peer connection.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Peer's public key (from Announce message).
    pub pubkey: PubKey,
    /// Current session state.
    pub state: PeerSessionState,
    /// Current access level.
    pub access_level: AccessLevel,
    /// Agreed protocol version.
    pub protocol_version: u8,
    /// Agreed unit (e.g., "bytes").
    pub unit: String,
    /// Agreed product ID.
    pub product_id: Option<Hash32>,
    /// Outbound channel state (AB direction).
    pub channel_ab: Option<ChannelState>,
    /// Inbound channel state (BA direction).
    pub channel_ba: Option<ChannelState>,
    /// Cumulative balance for outbound channel (AB direction).
    pub balance_ab: Amount,
    /// Cumulative balance for inbound channel (BA direction).
    pub balance_ba: Amount,
    /// Latest metering metrics from our side.
    pub our_metrics: PeerMetrics,
    /// Latest metering metrics reported by peer.
    pub their_metrics: PeerMetrics,
}

impl PeerInfo {
    /// Create a new PeerInfo in Initial state.
    pub fn new(pubkey: PubKey) -> Self {
        Self {
            pubkey,
            state: PeerSessionState::default(),
            access_level: AccessLevel::default(),
            protocol_version: 0,
            unit: String::new(),
            product_id: None,
            channel_ab: None,
            channel_ba: None,
            balance_ab: Amount::ZERO,
            balance_ba: Amount::ZERO,
            our_metrics: PeerMetrics::zero(),
            their_metrics: PeerMetrics::zero(),
        }
    }
}
