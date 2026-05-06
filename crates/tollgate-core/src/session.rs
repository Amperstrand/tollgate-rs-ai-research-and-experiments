//! Peer session handler — coordinates state machine, bootstrap, wallet, and adapter.
//!
//! `PeerSession` is the core message handler that ties together the peer state
//! machine, bootstrap token payment, wallet operations, and resource adapter
//! into a working peer session.
//!
//! The handler is transport-agnostic — it receives `Message`s and returns
//! `Message`s. The transport layer is responsible for delivery.

use std::sync::Arc;

use crate::access::AccessLevel;
use crate::adapter::ResourceAdapter;
use crate::bootstrap::{BootstrapIntervalResult, BootstrapSession};
use crate::config::ProductConfig;
use crate::metering::PeerMetrics;
use crate::peer::{PeerSessionState, PeerStateMachine};
use crate::protocol::{
    Accept, Announce, BootstrapAck, BootstrapStatus, BootstrapToken, Disconnect, IntervalRange,
    Message, MessageType, MintOption, PriceSheet, Product, PubKey, ReasonCode, Reject,
    MeteringReport,
};
use crate::wallet::Wallet;

/// Configuration for a [`PeerSession`].
pub struct SessionConfig {
    /// Our public key (sent in Announce).
    pub pubkey: PubKey,
    /// Protocol version we advertise.
    pub protocol_version: u8,
    /// Unit we advertise (e.g., "bytes").
    pub unit: String,
    /// Capability flags we advertise.
    pub capabilities: u32,
    /// Products we offer (empty for leaf/buyer nodes).
    pub products: Vec<ProductConfig>,
    /// Preferred metering interval in milliseconds.
    pub interval_ms: u32,
}

/// Cached pricing details for the accepted product.
#[allow(dead_code)]
struct AcceptedProduct {
    pricing_scale: u64,
    price_per_second: i64,
    price_per_unit: i64,
    mint_url: String,
}

/// Peer session handler — the core coordinator.
///
/// Coordinates state machine transitions, bootstrap token payments, wallet
/// operations, and resource adapter access control. Transport-agnostic:
/// receives `Message`s and returns `Message`s.
#[allow(dead_code)]
pub struct PeerSession<W: Wallet, A: ResourceAdapter> {
    sm: PeerStateMachine,
    bootstrap: Option<BootstrapSession>,
    wallet: Arc<W>,
    adapter: Arc<A>,
    config: SessionConfig,
    accepted_product: Option<AcceptedProduct>,
    our_last_metrics: PeerMetrics,
}

impl<W: Wallet, A: ResourceAdapter> PeerSession<W, A> {
    /// Create a new peer session.
    pub fn new(wallet: Arc<W>, adapter: Arc<A>, config: SessionConfig) -> Self {
        let sm = PeerStateMachine::new(PubKey([0u8; 33]));
        Self {
            sm,
            bootstrap: None,
            wallet,
            adapter,
            config,
            accepted_product: None,
            our_last_metrics: PeerMetrics::zero(),
        }
    }

    /// Create an Announce message from our configuration.
    pub fn create_announce(&self) -> Message {
        Message::Announce(Announce {
            msg_type: MessageType::Announce as u8,
            protocol_version: self.config.protocol_version,
            pubkey: self.config.pubkey.clone(),
            unit: self.config.unit.clone(),
            capabilities: self.config.capabilities,
        })
    }

    /// Create a PriceSheet message from our configured products.
    pub fn create_price_sheet(&self) -> Message {
        let products: Vec<Product> = self
            .config
            .products
            .iter()
            .map(|pc| Product {
                product_id: pc.product_id.clone(),
                extensions: pc.extensions.clone(),
                pricing_scale: pc.pricing_scale,
                mint_options: pc
                    .mint_options
                    .iter()
                    .map(|mc| MintOption {
                        option_id: mc.option_id.clone(),
                        mint_url: mc.mint_url.clone(),
                        price_per_second: mc.price_per_second,
                        price_per_unit: mc.price_per_unit,
                        mint_unit: mc.mint_unit.clone(),
                    })
                    .collect(),
            })
            .collect();

        Message::PriceSheet(PriceSheet {
            msg_type: MessageType::PriceSheet as u8,
            products,
            interval_range: IntervalRange([
                self.config.interval_ms / 2,
                self.config.interval_ms * 2,
            ]),
        })
    }

    /// Main message handler — dispatches to individual handlers.
    ///
    /// Returns a list of response messages (possibly empty).
    pub async fn handle_message(&mut self, msg: Message) -> Vec<Message> {
        match msg {
            Message::Announce(a) => self.handle_announce(a),
            Message::PriceSheet(s) => self.handle_price_sheet(s),
            Message::Accept(a) => self.handle_accept(a),
            Message::BootstrapToken(t) => self.handle_bootstrap_token(t).await,
            Message::MeteringReport(r) => self.handle_metering_report(r).await,
            Message::BootstrapAck(a) => self.handle_bootstrap_ack(a),
            Message::Reject(r) => self.handle_reject(r),
            Message::Disconnect(d) => self.handle_disconnect(d).await,
            _ => vec![],
        }
    }

    fn handle_announce(&mut self, announce: Announce) -> Vec<Message> {
        self.sm.info_mut().pubkey = announce.pubkey;
        if self
            .sm
            .on_announce(announce.protocol_version, announce.unit, announce.capabilities)
            .is_err()
        {
            return vec![Message::Reject(Reject {
                msg_type: MessageType::Reject as u8,
                rejected_type: MessageType::Announce as u8,
                reason_code: ReasonCode::Other,
                reason_text: Some("unexpected Announce".to_owned()),
            })];
        }
        vec![]
    }

    #[allow(clippy::unused_self)]
    fn handle_price_sheet(&mut self, _sheet: PriceSheet) -> Vec<Message> {
        vec![]
    }

    #[allow(clippy::needless_pass_by_value)]
    fn handle_accept(&mut self, accept: Accept) -> Vec<Message> {
        if self.sm.on_accept(accept.product_id.clone()).is_err() {
            return vec![Message::Reject(Reject {
                msg_type: MessageType::Reject as u8,
                rejected_type: MessageType::Accept as u8,
                reason_code: ReasonCode::Other,
                reason_text: Some("unexpected Accept".to_owned()),
            })];
        }

        for product in &self.config.products {
            if product.product_id == accept.product_id {
                for mint in &product.mint_options {
                    if mint.option_id == accept.option_id {
                        self.accepted_product = Some(AcceptedProduct {
                            pricing_scale: product.pricing_scale,
                            price_per_second: mint.price_per_second,
                            price_per_unit: mint.price_per_unit,
                            mint_url: mint.mint_url.clone(),
                        });
                        return vec![];
                    }
                }
            }
        }

        vec![Message::Reject(Reject {
            msg_type: MessageType::Reject as u8,
            rejected_type: MessageType::Accept as u8,
            reason_code: ReasonCode::Other,
            reason_text: Some("product not found".to_owned()),
        })]
    }

    async fn handle_bootstrap_token(&mut self, token: BootstrapToken) -> Vec<Message> {
        let state = self.sm.state().clone();
        match state {
            PeerSessionState::Priced => self.handle_initial_bootstrap(token).await,
            PeerSessionState::BootstrapActive => self.handle_top_up(token).await,
            _ => vec![Message::BootstrapAck(BootstrapAck {
                msg_type: MessageType::BootstrapAck as u8,
                status: BootstrapStatus::Rejected,
                reason: Some("unexpected BootstrapToken".to_owned()),
            })],
        }
    }

    async fn handle_initial_bootstrap(&mut self, token: BootstrapToken) -> Vec<Message> {
        match self.wallet.receive_token(&token.token).await {
            Ok(amount) => {
                if self.sm.on_bootstrap_token().is_err() {
                    return vec![Message::BootstrapAck(BootstrapAck {
                        msg_type: MessageType::BootstrapAck as u8,
                        status: BootstrapStatus::Rejected,
                        reason: Some("state machine rejected bootstrap token".to_owned()),
                    })];
                }

                let Some(ap) = &self.accepted_product else {
                    return vec![Message::BootstrapAck(BootstrapAck {
                        msg_type: MessageType::BootstrapAck as u8,
                        status: BootstrapStatus::Rejected,
                        reason: Some("no accepted product".to_owned()),
                    })];
                };

                let session = BootstrapSession::new(
                    amount,
                    ap.pricing_scale,
                    ap.price_per_second,
                    ap.price_per_unit,
                );

                self.bootstrap = Some(session);

                let _ = self
                    .adapter
                    .set_peer_access(&self.sm.info().pubkey.0, AccessLevel::Active)
                    .await;

                vec![Message::BootstrapAck(BootstrapAck {
                    msg_type: MessageType::BootstrapAck as u8,
                    status: BootstrapStatus::Accepted,
                    reason: None,
                })]
            }
            Err(e) => vec![Message::BootstrapAck(BootstrapAck {
                msg_type: MessageType::BootstrapAck as u8,
                status: BootstrapStatus::Rejected,
                reason: Some(e.to_string()),
            })],
        }
    }

    async fn handle_top_up(&mut self, token: BootstrapToken) -> Vec<Message> {
        match self.wallet.receive_token(&token.token).await {
            Ok(amount) => {
                if let Some(ref mut bs) = self.bootstrap {
                    bs.top_up(amount);
                }

                let access = self
                    .bootstrap
                    .as_ref()
                    .map_or(AccessLevel::Active, BootstrapSession::access_level);
                let _ = self
                    .adapter
                    .set_peer_access(&self.sm.info().pubkey.0, access)
                    .await;

                vec![Message::BootstrapAck(BootstrapAck {
                    msg_type: MessageType::BootstrapAck as u8,
                    status: BootstrapStatus::Accepted,
                    reason: None,
                })]
            }
            Err(e) => vec![Message::BootstrapAck(BootstrapAck {
                msg_type: MessageType::BootstrapAck as u8,
                status: BootstrapStatus::Rejected,
                reason: Some(e.to_string()),
            })],
        }
    }

    async fn handle_metering_report(&mut self, _report: MeteringReport) -> Vec<Message> {
        if self.sm.on_metering_report().is_err() {
            return vec![Message::Reject(Reject {
                msg_type: MessageType::Reject as u8,
                rejected_type: MessageType::MeteringReport as u8,
                reason_code: ReasonCode::Other,
                reason_text: Some("unexpected MeteringReport".to_owned()),
            })];
        }

        let Some(ref mut bs) = self.bootstrap else {
            return vec![];
        };

        let peer_id: &[u8] = &self.sm.info().pubkey.0;
        let Ok(metrics) = self.adapter.peer_metrics(peer_id).await else {
            return vec![];
        };

        match bs.process_interval(&metrics) {
            BootstrapIntervalResult::Ok { .. } => vec![],
            BootstrapIntervalResult::Exhausted { .. } => {
                let _ = self
                    .adapter
                    .set_peer_access(peer_id, AccessLevel::Suspended)
                    .await;
                vec![Message::Reject(Reject {
                    msg_type: MessageType::Reject as u8,
                    rejected_type: MessageType::MeteringReport as u8,
                    reason_code: ReasonCode::Other,
                    reason_text: Some("balance exhausted".to_owned()),
                })]
            }
            BootstrapIntervalResult::CounterWentBackwards => {
                vec![Message::Reject(Reject {
                    msg_type: MessageType::Reject as u8,
                    rejected_type: MessageType::MeteringReport as u8,
                    reason_code: ReasonCode::Other,
                    reason_text: Some("counter went backwards".to_owned()),
                })]
            }
        }
    }

    #[allow(clippy::unused_self)]
    fn handle_bootstrap_ack(&mut self, _ack: BootstrapAck) -> Vec<Message> {
        vec![]
    }

    #[allow(clippy::unused_self)]
    fn handle_reject(&mut self, _reject: Reject) -> Vec<Message> {
        vec![]
    }

    async fn handle_disconnect(&mut self, _disconnect: Disconnect) -> Vec<Message> {
        let _ = self.sm.on_disconnect();
        let _ = self
            .adapter
            .set_peer_access(&self.sm.info().pubkey.0, AccessLevel::None)
            .await;
        vec![Message::Disconnect(Disconnect {
            msg_type: MessageType::Disconnect as u8,
            reason_code: ReasonCode::Other,
        })]
    }

    /// Get the current peer session state.
    pub fn state(&self) -> &PeerSessionState {
        self.sm.state()
    }

    /// Returns true if the session has active payment (bootstrap or channel).
    pub fn is_active(&self) -> bool {
        matches!(
            self.sm.state(),
            PeerSessionState::BootstrapActive | PeerSessionState::ChannelReady
        )
    }
}
