//! The per-node TollGate state machine.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tollgate_protocol::{
    Accept, BootstrapAck, BootstrapToken, Disconnect, MessageType, PriceSheet, Reject,
};

use crate::access::AccessLevel;
use crate::action::Action;
use crate::event::Event;
use crate::metering::Counters;
use crate::peer::PeerId;
use crate::pricing::Price;
use crate::time::Millis;

/// Where one peer relationship sits in its lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerPhase {
    /// Connected, no payment yet — blocked.
    New,
    /// A bootstrap token was received and is being verified with the mint.
    BootstrapPending,
    /// Paid and allowed; metered.
    Active,
    /// Was active, balance exhausted — blocked, awaiting top-up.
    Suspended,
    /// Torn down.
    Closed,
}

/// Per-peer bookkeeping.
#[derive(Clone, Debug)]
struct PeerSession {
    phase: PeerPhase,
    /// Scaled milli-unit balance (scale = 1000). Bootstrap top-ups are additive.
    balance: u64,
    /// The last meter reading, or `None` until the first sample establishes the
    /// baseline (so a non-zero starting counter isn't charged retroactively).
    last_counters: Option<Counters>,
    /// When `last_counters` was taken.
    last_meter_at: Millis,
    /// The last PriceSheet received from this peer, if any. The host inspects
    /// this to decide whether to accept or reject — core does not negotiate.
    last_price_sheet: Option<PriceSheet>,
    /// The product this peer last accepted (set on receipt of an Accept).
    accepted_product: Option<[u8; 32]>,
}

impl PeerSession {
    fn new() -> Self {
        Self {
            phase: PeerPhase::New,
            balance: 0,
            last_counters: None,
            last_meter_at: Millis(0),
            last_price_sheet: None,
            accepted_product: None,
        }
    }
}

/// Holds one [`PeerSession`] per peer and turns [`Event`]s into [`Action`]s.
/// Pure and synchronous — see the crate-level docs.
#[derive(Debug, Default)]
pub struct Session {
    peers: BTreeMap<PeerId, PeerSession>,
    /// The rate this node charges peers for delivery (v1: one rate for all).
    price: Price,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the rate charged to peers. v1 uses a single price for all peers;
    /// per-peer pricing arrives with PriceSheet negotiation.
    pub fn set_price(&mut self, price: Price) {
        self.price = price;
    }

    /// Drive the state machine with one event. `now` is the host's monotonic
    /// clock; core never reads time on its own.
    ///
    /// This currently implements the bootstrap-only happy path (the first
    /// milestone: an IP deployment that sells access for plain Cashu tokens).
    /// Spilman channels, rollover, and netting are added on top later.
    pub fn handle(&mut self, event: Event, now: Millis) -> Vec<Action> {
        let mut actions = Vec::new();
        match event {
            Event::PeerConnected { peer } => {
                self.peers.insert(peer, PeerSession::new());
                // New peers start blocked until they pay (access-control default).
                actions.push(Action::SetAccess {
                    peer,
                    level: AccessLevel::None,
                });
            }

            Event::MessageReceived { peer, bytes } => {
                // Decode the message type and dispatch. The bootstrap-only path
                // turns a BootstrapToken (0x07) into a token-verification request
                // carrying the actual Cashu token bytes.
                // TODO: handle Announce, PriceSheet, Accept, MeteringReport, etc.
                match tollgate_protocol::peek_type(&bytes) {
                    Some(MessageType::BootstrapToken) => {
                        if let Ok(msg) = BootstrapToken::decode(&bytes) {
                            if let Some(peer_session) = self.peers.get_mut(&peer) {
                                peer_session.phase = PeerPhase::BootstrapPending;
                                actions.push(Action::VerifyBootstrapToken {
                                    peer,
                                    token: msg.token_bytes(),
                                });
                            }
                        }
                    }
                    Some(MessageType::Reject) => {
                        // A Reject rolls back any pending negotiation: the peer
                        // goes back to `New` (unless already torn down). If the
                        // peer was somehow `Active`, revoke access and metering.
                        // The decoded body is intentionally dropped — core has no
                        // policy engine yet; the host observes the state change.
                        if let Ok(_reject) = Reject::decode(&bytes) {
                            if let Some(peer_session) = self.peers.get_mut(&peer) {
                                if peer_session.phase != PeerPhase::Closed {
                                    let was_active = peer_session.phase == PeerPhase::Active;
                                    peer_session.phase = PeerPhase::New;
                                    if was_active {
                                        actions.push(Action::SetAccess {
                                            peer,
                                            level: AccessLevel::None,
                                        });
                                        actions.push(Action::StopMetering { peer });
                                    }
                                }
                            }
                        }
                    }
                    Some(MessageType::Disconnect) => {
                        // Orderly teardown — same cleanup as `PeerDisconnected`.
                        if Disconnect::decode(&bytes).is_ok() {
                            if let Some(peer_session) = self.peers.get_mut(&peer) {
                                peer_session.phase = PeerPhase::Closed;
                            }
                            actions.push(Action::SetAccess {
                                peer,
                                level: AccessLevel::None,
                            });
                            actions.push(Action::StopMetering { peer });
                            self.peers.remove(&peer);
                        }
                    }
                    Some(MessageType::PriceSheet) => {
                        // Store the offer for the host to inspect; core does not
                        // auto-accept or auto-reject. No actions emitted.
                        if let Ok(sheet) = PriceSheet::decode(&bytes) {
                            if let Some(peer_session) = self.peers.get_mut(&peer) {
                                peer_session.last_price_sheet = Some(sheet);
                            }
                        }
                    }
                    Some(MessageType::Accept) => {
                        // Record the accepted product id; no actions emitted.
                        if let Ok(accept) = Accept::decode(&bytes) {
                            if let Some(peer_session) = self.peers.get_mut(&peer) {
                                peer_session.accepted_product =
                                    accept.product_id.as_slice().try_into().ok();
                            }
                        }
                    }
                    _ => {}
                }
            }

            Event::BootstrapVerified { peer, amount, ok } => {
                if let Some(peer_session) = self.peers.get_mut(&peer) {
                    if ok {
                        peer_session.balance = peer_session.balance.saturating_add(amount);
                        peer_session.phase = PeerPhase::Active;
                        actions.push(Action::Send {
                            peer,
                            bytes: BootstrapAck::accepted().encode(),
                        });
                        actions.push(Action::SetAccess {
                            peer,
                            level: AccessLevel::Active,
                        });
                        actions.push(Action::StartMetering { peer });
                    } else {
                        peer_session.phase = PeerPhase::New;
                        actions.push(Action::Send {
                            peer,
                            bytes: BootstrapAck::rejected("token verification failed").encode(),
                        });
                        actions.push(Action::SetAccess {
                            peer,
                            level: AccessLevel::None,
                        });
                    }
                }
            }

            Event::MeterSample { peer, counters } => {
                let price = self.price;
                if let Some(peer_session) = self.peers.get_mut(&peer) {
                    if peer_session.phase == PeerPhase::Active {
                        match peer_session.last_counters {
                            // First reading after activation: establish the
                            // baseline, charge nothing.
                            None => {
                                peer_session.last_counters = Some(counters);
                                peer_session.last_meter_at = now;
                            }
                            Some(prev) => {
                                let delivered = counters.delivered.saturating_sub(prev.delivered);
                                let elapsed = now.since(peer_session.last_meter_at);
                                let cost = price.cost_scaled(elapsed, delivered);
                                peer_session.last_counters = Some(counters);
                                peer_session.last_meter_at = now;
                                peer_session.balance = peer_session.balance.saturating_sub(cost);
                                if peer_session.balance == 0 {
                                    peer_session.phase = PeerPhase::Suspended;
                                    actions.push(Action::SetAccess {
                                        peer,
                                        level: AccessLevel::Suspended,
                                    });
                                    actions.push(Action::StopMetering { peer });
                                }
                            }
                        }
                    }
                }
            }

            Event::PeerDisconnected { peer } => {
                if let Some(peer_session) = self.peers.get_mut(&peer) {
                    peer_session.phase = PeerPhase::Closed;
                }
                // Revoke firewall access and stop metering before forgetting the peer.
                actions.push(Action::SetAccess {
                    peer,
                    level: AccessLevel::None,
                });
                actions.push(Action::StopMetering { peer });
                self.peers.remove(&peer);
            }

            Event::Tick => {
                // TODO: per-interval netting, rollover checks, channel expiry.
            }
        }
        actions
    }

    /// Number of peers currently tracked (inspection/test helper).
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use tollgate_protocol::PublicKey;

    fn peer(byte: u8) -> PeerId {
        PeerId(PublicKey::from_bytes([byte; 33]))
    }

    #[test]
    fn bootstrap_only_happy_path() {
        let mut session = Session::new();
        let p = peer(1);

        let actions = session.handle(Event::PeerConnected { peer: p }, Millis(0));
        assert!(matches!(
            actions.as_slice(),
            [Action::SetAccess {
                level: AccessLevel::None,
                ..
            }]
        ));

        let token_frame =
            tollgate_protocol::BootstrapToken::new(b"cashuBtesttoken".to_vec()).encode();
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: token_frame,
            },
            Millis(1),
        );
        match actions.as_slice() {
            [Action::VerifyBootstrapToken { token, .. }] => {
                assert_eq!(token, b"cashuBtesttoken");
            }
            other => panic!("expected VerifyBootstrapToken, got {other:?}"),
        }

        let actions = session.handle(
            Event::BootstrapVerified {
                peer: p,
                amount: 5000,
                ok: true,
            },
            Millis(2),
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::SetAccess {
                level: AccessLevel::Active,
                ..
            }
        )));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StartMetering { .. }))
        );

        // An accepted BootstrapAck is queued for the wire.
        let ack_bytes = actions
            .iter()
            .find_map(|a| match a {
                Action::Send { bytes, .. } => Some(bytes.clone()),
                _ => None,
            })
            .expect("a Send action carrying a BootstrapAck");
        let ack = BootstrapAck::decode(&ack_bytes).expect("decode BootstrapAck");
        assert!(ack.is_accepted());

        assert_eq!(session.peer_count(), 1);
    }

    #[test]
    fn rejected_bootstrap_blocks_and_acks() {
        let mut session = Session::new();
        let p = peer(3);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));

        let actions = session.handle(
            Event::BootstrapVerified {
                peer: p,
                amount: 0,
                ok: false,
            },
            Millis(1),
        );

        assert!(actions.iter().any(|a| matches!(
            a,
            Action::SetAccess {
                level: AccessLevel::None,
                ..
            }
        )));
        let ack_bytes = actions
            .iter()
            .find_map(|a| match a {
                Action::Send { bytes, .. } => Some(bytes.clone()),
                _ => None,
            })
            .expect("a Send action carrying a BootstrapAck");
        assert!(
            !BootstrapAck::decode(&ack_bytes)
                .expect("decode")
                .is_accepted()
        );
    }

    #[test]
    fn metering_charges_deltas_and_suspends_when_exhausted() {
        let mut session = Session::new();
        session.set_price(Price {
            per_second: 0,
            per_unit: 1,
        }); // 1 unit = 1 milli
        let p = peer(7);

        session.handle(Event::PeerConnected { peer: p }, Millis(0));
        session.handle(
            Event::BootstrapVerified {
                peer: p,
                amount: 10,
                ok: true,
            },
            Millis(0),
        );

        let sample = |delivered| Event::MeterSample {
            peer: p,
            counters: Counters {
                delivered,
                received: 0,
            },
        };

        // First sample establishes the baseline — no charge, no actions.
        assert!(session.handle(sample(0), Millis(1000)).is_empty());
        // Deliver 4 units → cost 4, balance 10 → 6, still active.
        assert!(session.handle(sample(4), Millis(2000)).is_empty());
        // Deliver 6 more (cumulative 10) → cost 6, balance 6 → 0 → suspend.
        let actions = session.handle(sample(10), Millis(3000));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::SetAccess {
                level: AccessLevel::Suspended,
                ..
            }
        )));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StopMetering { .. }))
        );

        // Once suspended, further samples are ignored (no more actions).
        assert!(session.handle(sample(99), Millis(4000)).is_empty());
    }

    #[test]
    fn disconnect_forgets_the_peer() {
        let mut session = Session::new();
        let p = peer(2);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));
        let actions = session.handle(Event::PeerDisconnected { peer: p }, Millis(1));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StopMetering { .. }))
        );
        assert_eq!(session.peer_count(), 0);
    }

    #[test]
    fn reject_transitions_pending_peer_to_new() {
        let mut session = Session::new();
        let p = peer(4);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));

        // Drive the peer into BootstrapPending, as a token-bearing client would.
        let token_frame = tollgate_protocol::BootstrapToken::new(b"cashuBtoken".to_vec()).encode();
        session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: token_frame,
            },
            Millis(1),
        );
        assert_eq!(
            session.peers.get(&p).unwrap().phase,
            PeerPhase::BootstrapPending
        );

        // A Reject rolls the negotiation back to New without side-effect actions.
        let reject_frame = Reject::new(
            MessageType::BootstrapToken,
            tollgate_protocol::RejectReason::ProtocolVersionUnsupported,
        )
        .encode();
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: reject_frame,
            },
            Millis(2),
        );
        assert!(actions.is_empty());
        assert_eq!(session.peers.get(&p).unwrap().phase, PeerPhase::New);
    }

    #[test]
    fn reject_revokes_access_for_active_peer() {
        let mut session = Session::new();
        let p = peer(6);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));
        session.handle(
            Event::BootstrapVerified {
                peer: p,
                amount: 5000,
                ok: true,
            },
            Millis(1),
        );
        assert_eq!(session.peers.get(&p).unwrap().phase, PeerPhase::Active);

        // An active peer that gets rejected must lose access and drop to New.
        let reject_frame = Reject::new(
            MessageType::PriceSheet,
            tollgate_protocol::RejectReason::PriceTooHigh,
        )
        .encode();
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: reject_frame,
            },
            Millis(2),
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::SetAccess {
                level: AccessLevel::None,
                ..
            }
        )));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StopMetering { .. }))
        );
        assert_eq!(session.peers.get(&p).unwrap().phase, PeerPhase::New);
    }

    #[test]
    fn disconnect_cleans_up_peer() {
        let mut session = Session::new();
        let p = peer(5);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));

        let disconnect_frame =
            tollgate_protocol::Disconnect::new(tollgate_protocol::RejectReason::Other).encode();
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: disconnect_frame,
            },
            Millis(1),
        );

        assert!(actions.iter().any(|a| matches!(
            a,
            Action::SetAccess {
                level: AccessLevel::None,
                ..
            }
        )));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StopMetering { .. }))
        );
        assert_eq!(session.peer_count(), 0);
    }

    #[test]
    fn price_sheet_is_stored_on_receipt() {
        use tollgate_protocol::{IntervalRange, MintOption, PriceSheet, ProductEntry};

        let mut session = Session::new();
        let p = peer(8);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));

        let product = ProductEntry::new(
            [9u8; 32],
            vec![],
            1000,
            vec![MintOption::new(
                [1u8; 32],
                "https://mint.example",
                10,
                1,
                "sat",
            )],
        );
        let sheet = PriceSheet::new(vec![product], IntervalRange::new(1000, 5000));
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: sheet.encode(),
            },
            Millis(1),
        );

        // No actions emitted — host decides whether to accept or reject.
        assert!(actions.is_empty());

        let stored = session
            .peers
            .get(&p)
            .unwrap()
            .last_price_sheet
            .as_ref()
            .expect("price sheet stored");
        assert_eq!(stored.products.len(), 1);
        assert_eq!(stored.products[0].product_id.as_slice(), &[9u8; 32]);
        assert_eq!(stored.interval, IntervalRange::new(1000, 5000));
    }

    #[test]
    fn accept_is_stored_on_receipt() {
        use tollgate_protocol::{Accept, IntervalRange};

        let mut session = Session::new();
        let p = peer(9);
        session.handle(Event::PeerConnected { peer: p }, Millis(0));

        let accept = Accept::new([7u8; 32], [3u8; 32], IntervalRange::new(2000, 6000), vec![]);
        let actions = session.handle(
            Event::MessageReceived {
                peer: p,
                bytes: accept.encode(),
            },
            Millis(1),
        );

        // No actions emitted — just state update.
        assert!(actions.is_empty());

        assert_eq!(
            session.peers.get(&p).unwrap().accepted_product,
            Some([7u8; 32])
        );
    }
}
