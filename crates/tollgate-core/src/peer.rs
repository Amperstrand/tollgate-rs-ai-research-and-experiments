//! Peer session state machine and peer info.
//!
//! Defines the primary state machine for the protocol lifecycle.
//! Transitions are driven by incoming/outgoing messages.
//!
//! The state machine is pure — no I/O, no async, no side effects.
//! Each transition method validates the current state and returns
//! [`ProtocolError::UnexpectedMessage`] for illegal transitions.
//!
//! # State diagram
//!
//! ```text
//! Initial ──on_announce──► Announced
//!   │                         │
//!   │ on_disconnect           ├─on_accept──► Priced
//!   │                         │                │
//!   ▼                         │ on_disconnect  ├─on_bootstrap_token──► BootstrapActive
//!  Closed                     │                ├─on_channel_ready──► ChannelReady
//!                             ▼                │ on_disconnect
//!                            Closed             ▼
//!                                              Closed
//!
//! BootstrapActive ──on_channel_ready──► ChannelReady
//!      │    │                              │
//!      │    │ on_metering_report           ├─on_metering_report
//!      │    │ (no state change)            │ (no state change)
//!      │    │                              │
//!      │    └─on_channel_close──► Closing  └─on_channel_close──► Closing
//!      │                                     │
//!      └─on_disconnect──► Closed             └─on_disconnect──► Closed
//!
//! Closing ──on_disconnect──► Closed
//! ```
//!
//! See `docs/design/core/tollgate-protocol.md` for the full state diagram.

use crate::access::AccessLevel;
use crate::error::ProtocolError;
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
    /// Bootstrap token verified, token-based metering active.
    BootstrapActive,
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
    /// Capability flags from Announce.
    pub capabilities: u32,
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
            capabilities: 0,
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

/// Peer session state machine — validates transitions, tracks lifecycle.
///
/// Pure state tracking. No side effects, no I/O. The caller is responsible
/// for performing actual wallet/adapter operations based on state changes.
pub struct PeerStateMachine {
    info: PeerInfo,
}

impl PeerStateMachine {
    /// Create a new state machine for a peer (Initial state).
    pub fn new(pubkey: PubKey) -> Self {
        Self {
            info: PeerInfo::new(pubkey),
        }
    }

    /// Get a reference to the peer info.
    pub fn info(&self) -> &PeerInfo {
        &self.info
    }

    /// Get a mutable reference to the peer info.
    pub fn info_mut(&mut self) -> &mut PeerInfo {
        &mut self.info
    }

    /// Get the current state.
    pub fn state(&self) -> &PeerSessionState {
        &self.info.state
    }

    // --- Transition methods ---

    /// Announce received/sent. Valid only from [`PeerSessionState::Initial`].
    ///
    /// Sets protocol version, unit, and capabilities on the peer info.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `Initial` state.
    pub fn on_announce(
        &mut self,
        protocol_version: u8,
        unit: String,
        capabilities: u32,
    ) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::Initial => {
                self.info.protocol_version = protocol_version;
                self.info.unit = unit;
                self.info.capabilities = capabilities;
                self.info.state = PeerSessionState::Announced;
                Ok(())
            }
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "Announce".to_owned(),
            }),
        }
    }

    /// Accept sent/received — product selected from price sheet.
    /// Valid only from [`PeerSessionState::Announced`].
    ///
    /// Transitions to [`PeerSessionState::Priced`] and records the chosen product ID.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `Announced` state.
    pub fn on_accept(&mut self, product_id: Hash32) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::Announced => {
                self.info.product_id = Some(product_id);
                self.info.state = PeerSessionState::Priced;
                Ok(())
            }
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "Accept".to_owned(),
            }),
        }
    }

    /// Bootstrap token received and verified. Valid only from [`PeerSessionState::Priced`].
    ///
    /// Transitions to [`PeerSessionState::BootstrapActive`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `Priced` state.
    pub fn on_bootstrap_token(&mut self) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::Priced => {
                self.info.state = PeerSessionState::BootstrapActive;
                Ok(())
            }
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "BootstrapToken".to_owned(),
            }),
        }
    }

    /// Channel ready confirmed (both directions).
    /// Valid from [`PeerSessionState::Priced`] or [`PeerSessionState::BootstrapActive`].
    ///
    /// Transitions to [`PeerSessionState::ChannelReady`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `Priced` or `BootstrapActive` state.
    pub fn on_channel_ready(&mut self) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::Priced | PeerSessionState::BootstrapActive => {
                self.info.state = PeerSessionState::ChannelReady;
                Ok(())
            }
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "ChannelReady".to_owned(),
            }),
        }
    }

    /// Metering report received. Valid only from [`PeerSessionState::ChannelReady`]
    /// or [`PeerSessionState::BootstrapActive`].
    ///
    /// No state change — validates the message is legal in the current state.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `ChannelReady` or `BootstrapActive` state.
    pub fn on_metering_report(&mut self) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::ChannelReady | PeerSessionState::BootstrapActive => Ok(()),
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "MeteringReport".to_owned(),
            }),
        }
    }

    /// Channel close initiated. Valid from [`PeerSessionState::ChannelReady`]
    /// or [`PeerSessionState::BootstrapActive`].
    ///
    /// Transitions to [`PeerSessionState::Closing`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if not in `ChannelReady` or `BootstrapActive` state.
    pub fn on_channel_close(&mut self) -> Result<(), ProtocolError> {
        match self.info.state {
            PeerSessionState::ChannelReady | PeerSessionState::BootstrapActive => {
                self.info.state = PeerSessionState::Closing;
                Ok(())
            }
            ref s => Err(ProtocolError::UnexpectedMessage {
                state: format!("{s:?}"),
                got: "ChannelClose".to_owned(),
            }),
        }
    }

    /// Disconnect — valid from any state. Transitions to [`PeerSessionState::Closed`].
    ///
    /// Idempotent: calling from Closed stays Closed.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub fn on_disconnect(&mut self) -> Result<(), ProtocolError> {
        self.info.state = PeerSessionState::Closed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pubkey() -> PubKey {
        PubKey([0x02; 33])
    }

    fn test_product_id() -> Hash32 {
        Hash32([0x11; 32])
    }

    fn test_product_id_b() -> Hash32 {
        Hash32([0x22; 32])
    }

    // --- Happy path tests ---

    #[test]
    fn normal_path() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        assert_eq!(sm.state(), &PeerSessionState::Initial);

        sm.on_announce(1, "bytes".to_owned(), 0x01).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Announced);

        sm.on_accept(test_product_id()).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Priced);

        sm.on_channel_ready().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);

        sm.on_channel_close().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closing);

        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    fn zero_price_fast_path() {
        // Same as normal but no bootstrap — direct from Priced to ChannelReady.
        let mut sm = PeerStateMachine::new(test_pubkey());

        sm.on_announce(2, "bytes".to_owned(), 0).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Announced);

        sm.on_accept(test_product_id()).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Priced);

        sm.on_channel_ready().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);

        sm.on_channel_close().unwrap();
        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    fn bootstrap_path() {
        let mut sm = PeerStateMachine::new(test_pubkey());

        sm.on_announce(1, "bytes".to_owned(), 0x01).unwrap();
        sm.on_accept(test_product_id()).unwrap();

        sm.on_bootstrap_token().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);

        sm.on_channel_close().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closing);

        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    fn bootstrap_upgrade() {
        let mut sm = PeerStateMachine::new(test_pubkey());

        sm.on_announce(1, "bytes".to_owned(), 0x01).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        sm.on_bootstrap_token().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);

        // Upgrade from bootstrap to Spilman channels
        sm.on_channel_ready().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);

        sm.on_channel_close().unwrap();
        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn disconnect_from_every_state() {
        let states: Vec<Box<dyn Fn(&mut PeerStateMachine)>> = vec![
            Box::new(|sm| {
                // Initial — just created
                assert_eq!(sm.state(), &PeerSessionState::Initial);
            }),
            Box::new(|sm| {
                sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
                assert_eq!(sm.state(), &PeerSessionState::Announced);
            }),
            Box::new(|sm| {
                sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
                sm.on_accept(test_product_id()).unwrap();
                assert_eq!(sm.state(), &PeerSessionState::Priced);
            }),
            Box::new(|sm| {
                sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
                sm.on_accept(test_product_id()).unwrap();
                sm.on_bootstrap_token().unwrap();
                assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);
            }),
            Box::new(|sm| {
                sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
                sm.on_accept(test_product_id()).unwrap();
                sm.on_channel_ready().unwrap();
                assert_eq!(sm.state(), &PeerSessionState::ChannelReady);
            }),
            Box::new(|sm| {
                sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
                sm.on_accept(test_product_id()).unwrap();
                sm.on_channel_ready().unwrap();
                sm.on_channel_close().unwrap();
                assert_eq!(sm.state(), &PeerSessionState::Closing);
            }),
        ];

        for setup in states {
            let mut sm = PeerStateMachine::new(test_pubkey());
            setup(&mut sm);
            sm.on_disconnect().unwrap();
            assert_eq!(sm.state(), &PeerSessionState::Closed);
        }
    }

    #[test]
    fn invalid_transitions() {
        let pubkey = test_pubkey();

        // on_announce from Announced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        let err = sm.on_announce(2, "bytes".to_owned(), 0).unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_announce from Priced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        let err = sm.on_announce(2, "bytes".to_owned(), 0).unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_accept from Initial
        let mut sm = PeerStateMachine::new(pubkey.clone());
        let err = sm.on_accept(test_product_id()).unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_accept from ChannelReady
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        sm.on_channel_ready().unwrap();
        let err = sm.on_accept(test_product_id_b()).unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_ready from Announced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        let err = sm.on_channel_ready().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_ready from Initial
        let mut sm = PeerStateMachine::new(pubkey.clone());
        let err = sm.on_channel_ready().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_bootstrap_token from Announced (need Accept first)
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        let err = sm.on_bootstrap_token().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_bootstrap_token from ChannelReady
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        sm.on_channel_ready().unwrap();
        let err = sm.on_bootstrap_token().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_metering_report from Initial
        let mut sm = PeerStateMachine::new(pubkey.clone());
        let err = sm.on_metering_report().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_metering_report from Announced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        let err = sm.on_metering_report().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_metering_report from Priced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        let err = sm.on_metering_report().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_close from Initial
        let mut sm = PeerStateMachine::new(pubkey.clone());
        let err = sm.on_channel_close().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_close from Announced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        let err = sm.on_channel_close().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_close from Priced
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        let err = sm.on_channel_close().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));

        // on_channel_close from Closed
        let mut sm = PeerStateMachine::new(pubkey.clone());
        sm.on_disconnect().unwrap();
        let err = sm.on_channel_close().unwrap_err();
        assert!(matches!(err, ProtocolError::UnexpectedMessage { .. }));
    }

    #[test]
    fn state_queryable() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        assert_eq!(sm.state(), &PeerSessionState::Initial);

        sm.on_announce(1, "bytes".to_owned(), 0x01).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Announced);

        sm.on_accept(test_product_id()).unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Priced);

        sm.on_bootstrap_token().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);

        sm.on_channel_ready().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);

        sm.on_channel_close().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closing);

        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    fn disconnect_is_idempotent() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);

        // Second disconnect from Closed stays Closed
        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);

        // Third disconnect still works
        sm.on_disconnect().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::Closed);
    }

    #[test]
    fn metering_report_no_state_change() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        sm.on_channel_ready().unwrap();

        sm.on_metering_report().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);

        sm.on_metering_report().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::ChannelReady);
    }

    #[test]
    fn bootstrap_metering_no_state_change() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        sm.on_accept(test_product_id()).unwrap();
        sm.on_bootstrap_token().unwrap();

        sm.on_metering_report().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);

        sm.on_metering_report().unwrap();
        assert_eq!(sm.state(), &PeerSessionState::BootstrapActive);
    }

    #[test]
    fn announce_sets_peer_info_fields() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        sm.on_announce(2, "watt_hours".to_owned(), 0xFF).unwrap();

        assert_eq!(sm.info().protocol_version, 2);
        assert_eq!(sm.info().unit, "watt_hours");
        assert_eq!(sm.info().capabilities, 0xFF);
    }

    #[test]
    fn accept_sets_product_id() {
        let mut sm = PeerStateMachine::new(test_pubkey());
        sm.on_announce(1, "bytes".to_owned(), 0).unwrap();
        assert_eq!(sm.info().product_id, None);

        sm.on_accept(test_product_id()).unwrap();
        assert_eq!(sm.info().product_id, Some(test_product_id()));
    }

    #[test]
    fn info_and_info_mut_accessors() {
        let mut sm = PeerStateMachine::new(test_pubkey());

        // Immutable access
        assert_eq!(sm.info().protocol_version, 0);

        // Mutable access
        sm.info_mut().protocol_version = 99;
        assert_eq!(sm.info().protocol_version, 99);
    }
}
