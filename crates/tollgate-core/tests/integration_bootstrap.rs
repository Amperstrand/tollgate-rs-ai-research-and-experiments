use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tollgate_core::access::AccessLevel;
use tollgate_core::adapter::ResourceAdapter;
use tollgate_core::config::{ProductConfig, ProductMintConfig};
use tollgate_core::error::{AdapterError, WalletError};
use tollgate_core::metering::PeerMetrics;
use tollgate_core::peer::PeerSessionState;
use tollgate_core::protocol::*;
use tollgate_core::session::{PeerSession, SessionConfig};
use tollgate_core::types::{
    Amount, ChannelFundParams, ChannelSecret, FundingProof, SettlementResult,
};
use tollgate_core::wallet::Wallet;

// ---------------------------------------------------------------------------
// MockWallet
// ---------------------------------------------------------------------------

struct MockWallet {
    balance: Mutex<u64>,
}

impl MockWallet {
    fn new(initial_balance: u64) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
        }
    }
}

#[allow(clippy::manual_async_fn)]
impl Wallet for MockWallet {
    fn receive_token(
        &self,
        token: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        let result = if token.len() < 8 {
            Err(WalletError::TokenRejected("token too short".to_owned()))
        } else {
            let amount = u64::from_be_bytes(token[..8].try_into().expect("checked length"));
            if amount == 0 {
                Err(WalletError::TokenRejected("zero amount".to_owned()))
            } else {
                let mut balance = self.balance.lock().expect("lock");
                *balance += amount;
                Ok(Amount(amount))
            }
        };
        async move { result }
    }

    fn create_token(
        &self,
        amount: Amount,
        _mint_url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, WalletError>> + Send {
        let result = {
            let mut balance = self.balance.lock().expect("lock");
            if *balance < amount.0 {
                Err(WalletError::Internal("insufficient balance".to_owned()))
            } else {
                *balance -= amount.0;
                Ok(amount.0.to_be_bytes().to_vec())
            }
        };
        async move { result }
    }

    fn fund_channel(
        &self,
        _: &ChannelFundParams,
        _: &ChannelSecret,
    ) -> impl Future<Output = Result<FundingProof, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn verify_funding(
        &self,
        _: &Hash32,
        _: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn sign_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
    ) -> impl Future<Output = Result<Signature, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn verify_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
        _: &Signature,
    ) -> impl Future<Output = Result<(), WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn settle_channel(
        &self,
        _: &Hash32,
    ) -> impl Future<Output = Result<SettlementResult, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn mint_reachable(&self, _: &str) -> impl Future<Output = Result<bool, WalletError>> + Send {
        async { Ok(true) }
    }

    fn balance(&self) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        let amount = *self.balance.lock().expect("lock");
        async move { Ok(Amount(amount)) }
    }

    fn compute_channel_secret(
        &self,
        _: &PubKey,
    ) -> impl Future<Output = Result<ChannelSecret, WalletError>> + Send {
        async { Ok(ChannelSecret([0u8; 32])) }
    }
}

// ---------------------------------------------------------------------------
// MockAdapter
// ---------------------------------------------------------------------------

struct MockAdapter {
    access_levels: Mutex<HashMap<Vec<u8>, AccessLevel>>,
    metrics: Mutex<PeerMetrics>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            access_levels: Mutex::new(HashMap::new()),
            metrics: Mutex::new(PeerMetrics::zero()),
        }
    }

    fn set_metrics(&self, m: PeerMetrics) {
        *self.metrics.lock().expect("lock") = m;
    }

    fn get_access_level(&self, peer_id: &[u8]) -> Option<AccessLevel> {
        self.access_levels
            .lock()
            .expect("lock")
            .get(peer_id)
            .copied()
    }
}

impl ResourceAdapter for MockAdapter {
    fn set_peer_access(
        &self,
        peer_id: &[u8],
        level: AccessLevel,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send {
        let peer_id = peer_id.to_owned();
        async move {
            self.access_levels
                .lock()
                .expect("lock")
                .insert(peer_id, level);
            Ok(())
        }
    }

    fn peer_metrics(
        &self,
        _: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send {
        let metrics = self.metrics.lock().expect("lock").clone();
        async move { Ok(metrics) }
    }

    fn subscribe_meter(
        &self,
        peer_id: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send {
        self.peer_metrics(peer_id)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_token(amount: u64) -> Vec<u8> {
    amount.to_be_bytes().to_vec()
}

fn provider_pubkey() -> PubKey {
    PubKey([0x01; 33])
}

fn client_pubkey() -> PubKey {
    PubKey([0x02; 33])
}

fn test_product_id() -> Hash32 {
    Hash32([0x11; 32])
}

fn test_option_id() -> Hash32 {
    Hash32([0x22; 32])
}

fn provider_config() -> SessionConfig {
    SessionConfig {
        pubkey: provider_pubkey(),
        protocol_version: 1,
        unit: "bytes".to_owned(),
        capabilities: 0x01,
        products: vec![ProductConfig {
            product_id: test_product_id(),
            extensions: vec![],
            pricing_scale: 1000,
            mint_options: vec![ProductMintConfig {
                option_id: test_option_id(),
                mint_url: "https://mint.example.com".to_owned(),
                price_per_second: 10,
                price_per_unit: 1,
                mint_unit: "sat".to_owned(),
            }],
        }],
        interval_ms: 5000,
    }
}

fn client_config() -> SessionConfig {
    SessionConfig {
        pubkey: client_pubkey(),
        protocol_version: 1,
        unit: "bytes".to_owned(),
        capabilities: 0x01,
        products: vec![],
        interval_ms: 5000,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn bootstrap_lifecycle() {
    let provider_wallet = Arc::new(MockWallet::new(0));
    let provider_adapter = Arc::new(MockAdapter::new());
    let client_wallet = Arc::new(MockWallet::new(200));
    let client_adapter = Arc::new(MockAdapter::new());

    let mut provider =
        PeerSession::new(provider_wallet, provider_adapter.clone(), provider_config());
    let mut client = PeerSession::new(client_wallet, client_adapter, client_config());

    // Step 1: Announce exchange
    let provider_announce = provider.create_announce();
    let msgs = client.handle_message(provider_announce).await;
    assert!(msgs.is_empty());
    assert_eq!(client.state(), &PeerSessionState::Announced);

    let client_announce = client.create_announce();
    let msgs = provider.handle_message(client_announce).await;
    assert!(msgs.is_empty());
    assert_eq!(provider.state(), &PeerSessionState::Announced);

    // Step 2: PriceSheet
    let price_sheet = provider.create_price_sheet();
    let msgs = client.handle_message(price_sheet).await;
    assert!(msgs.is_empty());

    // Step 3: Accept — client selects the product
    let accept = Message::Accept(Accept {
        msg_type: MessageType::Accept as u8,
        product_id: test_product_id(),
        option_id: test_option_id(),
        interval_range: IntervalRange([2500, 10000]),
        channel_funding: vec![],
    });
    let msgs = provider.handle_message(accept).await;
    assert!(msgs.is_empty());
    assert_eq!(provider.state(), &PeerSessionState::Priced);

    // Step 4: BootstrapToken (100 sats)
    let bootstrap_token = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: make_token(100),
    });
    let msgs = provider.handle_message(bootstrap_token).await;
    assert_eq!(msgs.len(), 1);
    match &msgs[0] {
        Message::BootstrapAck(ack) => {
            assert_eq!(ack.status, BootstrapStatus::Accepted);
            assert!(ack.reason.is_none());
        }
        other => panic!("expected BootstrapAck, got {other:?}"),
    }
    assert_eq!(provider.state(), &PeerSessionState::BootstrapActive);
    assert!(provider.is_active());

    // Verify adapter granted active access for the client's pubkey
    assert_eq!(
        provider_adapter.get_access_level(&client_pubkey().0),
        Some(AccessLevel::Active)
    );

    // Step 5: MeteringReport — 5 seconds elapsed, 1000 units delivered
    provider_adapter.set_metrics(PeerMetrics {
        elapsed_ms: 5000,
        delivered: 1000,
        received: 500,
    });

    let metering_report = Message::MeteringReport(MeteringReport {
        msg_type: MessageType::MeteringReport as u8,
        elapsed_ms: 5000,
        delivered: 1000,
        received: 500,
        new_product_id: None,
        new_pricing: None,
    });
    let msgs = provider.handle_message(metering_report).await;
    assert!(msgs.is_empty(), "provider should still have balance");

    // cost_scaled = 5*10 + 1000*1 = 1050
    // balance = 100*1000 - 1050 = 98950

    // Step 6: Drain the balance with repeated intervals.
    // Each interval: +5000ms, +1000 units → cost = 1050 scaled.
    // 98950 / 1050 ≈ 94.2, so ~95 more intervals needed.
    let mut exhausted = false;
    for i in 1..=100u64 {
        let elapsed = 5000 + i * 5000;
        let delivered = 1000 + i * 1000;
        provider_adapter.set_metrics(PeerMetrics {
            elapsed_ms: elapsed,
            delivered,
            received: 500 + i * 500,
        });

        let report = Message::MeteringReport(MeteringReport {
            msg_type: MessageType::MeteringReport as u8,
            elapsed_ms: elapsed,
            delivered,
            received: 500 + i * 500,
            new_product_id: None,
            new_pricing: None,
        });
        let msgs = provider.handle_message(report).await;

        if !msgs.is_empty() {
            match &msgs[0] {
                Message::Reject(r) => {
                    assert_eq!(r.reason_text, Some("balance exhausted".to_owned()));
                }
                other => panic!("expected Reject, got {other:?}"),
            }
            exhausted = true;
            break;
        }
    }
    assert!(exhausted, "balance should have been exhausted");

    // Verify access suspended
    assert_eq!(
        provider_adapter.get_access_level(&client_pubkey().0),
        Some(AccessLevel::Suspended)
    );

    // Step 7: Top-up (50 sats)
    let top_up = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: make_token(50),
    });
    let msgs = provider.handle_message(top_up).await;
    assert_eq!(msgs.len(), 1);
    match &msgs[0] {
        Message::BootstrapAck(ack) => {
            assert_eq!(ack.status, BootstrapStatus::Accepted);
        }
        other => panic!("expected BootstrapAck, got {other:?}"),
    }

    // Verify access restored to Active
    assert_eq!(
        provider_adapter.get_access_level(&client_pubkey().0),
        Some(AccessLevel::Active)
    );

    // Step 8: Disconnect
    let disconnect = Message::Disconnect(Disconnect {
        msg_type: MessageType::Disconnect as u8,
        reason_code: ReasonCode::Other,
    });
    let msgs = provider.handle_message(disconnect).await;
    assert_eq!(msgs.len(), 1);
    match &msgs[0] {
        Message::Disconnect(d) => {
            assert_eq!(d.reason_code, ReasonCode::Other);
        }
        other => panic!("expected Disconnect, got {other:?}"),
    }
    assert_eq!(provider.state(), &PeerSessionState::Closed);
    assert!(!provider.is_active());

    // Verify adapter revoked access
    assert_eq!(
        provider_adapter.get_access_level(&client_pubkey().0),
        Some(AccessLevel::None)
    );
}

#[tokio::test]
async fn bootstrap_token_rejected_by_wallet() {
    let provider_wallet = Arc::new(MockWallet::new(0));
    let provider_adapter = Arc::new(MockAdapter::new());

    let mut provider = PeerSession::new(provider_wallet, provider_adapter, provider_config());

    // Advance provider to Priced state
    let _msgs = provider
        .handle_message(Message::Announce(Announce {
            msg_type: MessageType::Announce as u8,
            protocol_version: 1,
            pubkey: client_pubkey(),
            unit: "bytes".to_owned(),
            capabilities: 0x01,
        }))
        .await;
    assert_eq!(provider.state(), &PeerSessionState::Announced);

    let _msgs = provider
        .handle_message(Message::Accept(Accept {
            msg_type: MessageType::Accept as u8,
            product_id: test_product_id(),
            option_id: test_option_id(),
            interval_range: IntervalRange([2500, 10000]),
            channel_funding: vec![],
        }))
        .await;
    assert_eq!(provider.state(), &PeerSessionState::Priced);

    // Send a zero-amount token — MockWallet rejects these
    let zero_token = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: make_token(0),
    });
    let msgs = provider.handle_message(zero_token).await;
    assert_eq!(msgs.len(), 1);
    match &msgs[0] {
        Message::BootstrapAck(ack) => {
            assert_eq!(ack.status, BootstrapStatus::Rejected);
            assert!(ack.reason.is_some());
        }
        other => panic!("expected BootstrapAck, got {other:?}"),
    }

    // State should remain Priced (not transitioned to BootstrapActive)
    assert_eq!(provider.state(), &PeerSessionState::Priced);
}
