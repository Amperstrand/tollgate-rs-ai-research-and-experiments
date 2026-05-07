//! CDK Integration Test — Verbose Protocol-Aware Lifecycle
//!
//! Tests the full TollGate v2 bootstrap payment lifecycle using real Cashu
//! tokens minted from testnut.cashu.space (Nutshell FakeWallet).
//!
//! Per docs/design/core/tollgate-bootstrap.md: Bootstrap token payment flow.
//! Per docs/design/core/tollgate-protocol.md: Message sequence.
//! Per Cashu NUT-00: Token encoding (V4 cashuB).
//!
//! Run with:
//!   cargo test -p tollgate-net --test cdk_integration -- --ignored --nocapture

use std::sync::Arc;

use tollgate_core::access::AccessLevel;
use tollgate_core::config::{ProductConfig, ProductMintConfig};
use tollgate_core::metering::PeerMetrics;
use tollgate_core::protocol::{
    Accept, BootstrapStatus, BootstrapToken, Disconnect, Hash32, IntervalRange, Message,
    MessageType, MeteringReport, PubKey, ReasonCode,
};
use tollgate_core::session::{PeerSession, SessionConfig};
use tollgate_core::types::Amount;
use tollgate_core::wallet::Wallet;

use tollgate_net::cdk_wallet::CdkWallet;
use tollgate_net::mock::MockAdapter;

const MINT_URL: &str = "https://testnut.cashu.space";

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

fn provider_session_config() -> SessionConfig {
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
                mint_url: MINT_URL.to_owned(),
                price_per_second: 10,
                price_per_unit: 1,
                mint_unit: "sat".to_owned(),
            }],
        }],
        interval_ms: 5000,
    }
}

fn client_session_config() -> SessionConfig {
    SessionConfig {
        pubkey: client_pubkey(),
        protocol_version: 1,
        unit: "bytes".to_owned(),
        capabilities: 0x01,
        products: vec![],
        interval_ms: 5000,
    }
}

#[tokio::test]
#[ignore = "requires network access to testnut.cashu.space"]
#[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
async fn cdk_bootstrap_lifecycle() {
    let _guard = tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=debug,tollgate_core=debug")
        .with_test_writer()
        .try_init();

    tracing::info!("╔══════════════════════════════════════════════════════════════╗");
    tracing::info!("║  TollGate v2 CDK Integration Test                           ║");
    tracing::info!("║  Mint: https://testnut.cashu.space (Nutshell FakeWallet)    ║");
    tracing::info!("║  Per docs/design/core/tollgate-bootstrap.md §3              ║");
    tracing::info!("║  Per Cashu NUT-00: Token encoding (V4 cashuB)               ║");
    tracing::info!("╚══════════════════════════════════════════════════════════════╝");

    // ─── Step 1: Create Provider Wallet ───
    tracing::info!("");
    tracing::info!("━━━ Step 1: Create Provider Wallet ━━━");
    tracing::info!("[NUT-06] Connecting provider wallet to {MINT_URL}");
    let provider_wallet = Arc::new(
        CdkWallet::new(MINT_URL, rand::random::<[u8; 64]>())
            .await
            .expect("provider wallet init"),
    );
    let provider_bal = provider_wallet
        .total_balance()
        .await
        .expect("provider balance");
    tracing::info!("[NUT-06] Provider wallet connected. Balance: {provider_bal} sat");

    // ─── Step 2: Create Client Wallet + Mint Tokens ───
    tracing::info!("");
    tracing::info!("━━━ Step 2: Create Client Wallet + Mint Tokens ━━━");
    tracing::info!("[NUT-04] Minting 200 sat from testnut (FakeWallet auto-pays)");
    let client_wallet = Arc::new(
        CdkWallet::new(MINT_URL, rand::random::<[u8; 64]>())
            .await
            .expect("client wallet init"),
    );
    client_wallet
        .mint_test_tokens(200)
        .await
        .expect("mint test tokens");
    let client_bal = client_wallet.total_balance().await.expect("client balance");
    tracing::info!("[NUT-04] Client balance after minting: {client_bal} sat");
    assert!(
        client_bal >= 200,
        "client should have at least 200 sat, got {client_bal}"
    );

    // ─── Step 3: Create Provider PeerSession ───
    tracing::info!("");
    tracing::info!("━━━ Step 3: Provider Session Setup ━━━");
    tracing::info!("Per docs/design/core/tollgate-protocol.md §2: Provider advertises products");
    tracing::info!("Product: price_per_second=10, price_per_unit=1, scale=1000");
    let provider_adapter = Arc::new(MockAdapter::new());
    let provider_config = provider_session_config();
    let mut provider_session = PeerSession::new(
        provider_wallet.clone(),
        provider_adapter.clone(),
        provider_config,
    );
    tracing::info!(
        "Provider PeerSession created (state: {:?})",
        provider_session.state()
    );

    // ─── Step 4: Create Client PeerSession ───
    tracing::info!("");
    tracing::info!("━━━ Step 4: Client Session Setup ━━━");
    let client_adapter = Arc::new(MockAdapter::new());
    let client_config = client_session_config();
    let mut client_session = PeerSession::new(client_wallet.clone(), client_adapter, client_config);
    tracing::info!(
        "Client PeerSession created (state: {:?})",
        client_session.state()
    );

    // ─── Step 5: Announce Exchange ───
    tracing::info!("");
    tracing::info!("━━━ Step 5: Announce Exchange ━━━");
    tracing::info!("Per docs/design/core/tollgate-protocol.md §3.1: Peer announcement");
    let client_announce = client_session.create_announce();
    tracing::info!("Client → Provider: Announce (version=1, unit=bytes, capabilities=0x01)");

    let msgs = provider_session.handle_message(client_announce).await;
    tracing::info!(
        "Provider handled Announce → {} response messages",
        msgs.len()
    );
    assert!(msgs.is_empty(), "Announce produces no direct response");
    tracing::info!(
        "Provider state after Announce: {:?}",
        provider_session.state()
    );

    let provider_announce = provider_session.create_announce();
    let _msgs = client_session.handle_message(provider_announce).await;
    tracing::info!("Client handled provider's Announce");

    let price_sheet = provider_session.create_price_sheet();
    tracing::info!(
        "Provider → Client: PriceSheet ({} products)",
        price_sheet.products_count()
    );

    macro_rules! price_sheet_products {
        () => {{
            if let Message::PriceSheet(ref sheet) = price_sheet {
                sheet.products.len()
            } else {
                0
            }
        }};
    }
    let num_products = price_sheet_products!();
    tracing::info!("PriceSheet contains {num_products} product(s)");
    let _msgs = client_session.handle_message(price_sheet).await;

    // ─── Step 6: Product Selection (Accept) ───
    tracing::info!("");
    tracing::info!("━━━ Step 6: Product Selection (Accept) ━━━");
    tracing::info!("Per docs/design/core/tollgate-protocol.md §3.2: Client selects product");
    let pid = test_product_id();
    let oid = test_option_id();
    tracing::info!(
        "Client selects: product_id={:?}..., option_id={:?}...",
        &pid.0[..4],
        &oid.0[..4]
    );
    let accept = Message::Accept(Accept {
        msg_type: MessageType::Accept as u8,
        product_id: test_product_id(),
        option_id: test_option_id(),
        interval_range: IntervalRange([2500, 10000]),
        channel_funding: vec![],
    });
    let msgs = provider_session.handle_message(accept).await;
    assert!(msgs.is_empty(), "Accept produces no direct response");
    tracing::info!(
        "Provider state after Accept: {:?}",
        provider_session.state()
    );
    assert_eq!(
        format!("{:?}", provider_session.state()),
        "Priced".to_string(),
        "provider should be in Priced state"
    );

    // ─── Step 7: Bootstrap Token (Real Cashu!) ───
    tracing::info!("");
    tracing::info!("━━━ Step 7: Bootstrap Token Payment (REAL CASHU!) ━━━");
    tracing::info!("Per docs/design/core/tollgate-bootstrap.md §3: Token-based bootstrap");
    tracing::info!("[NUT-00] Creating cashuB V4 token for 100 sat");

    let token_amount = Amount(100);
    let token_bytes = client_wallet
        .create_token(token_amount, MINT_URL)
        .await
        .expect("create Cashu token");

    tracing::info!(
        "[NUT-00] Token created: {} bytes (V4 cashuB encoding)",
        token_bytes.len()
    );
    tracing::info!(
        "[NUT-00] Token preview: {:?}...",
        &token_bytes[..token_bytes.len().min(40)]
    );

    let bootstrap_token = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: token_bytes,
    });

    let pre_bal = provider_wallet
        .total_balance()
        .await
        .expect("pre-receive balance");
    tracing::info!("Provider balance BEFORE receive: {pre_bal} sat");

    let msgs = provider_session.handle_message(bootstrap_token).await;
    assert_eq!(msgs.len(), 1, "should get exactly one response");

    let post_bal = provider_wallet
        .total_balance()
        .await
        .expect("post-receive balance");
    tracing::info!(
        "Provider balance AFTER receive: {post_bal} sat (delta: {} sat)",
        post_bal - pre_bal
    );

    match &msgs[0] {
        Message::BootstrapAck(ack) => {
            tracing::info!(
                "BootstrapAck: status={:?}, reason={:?}",
                ack.status,
                ack.reason
            );
            assert_eq!(ack.status, BootstrapStatus::Accepted);
            assert!(ack.reason.is_none());
        }
        other => panic!("expected BootstrapAck, got {other:?}"),
    }

    tracing::info!("Provider wallet received REAL Cashu token from testnut!");
    tracing::info!("Provider state: {:?}", provider_session.state());
    assert!(provider_session.is_active(), "session should be active");

    let client_bal_after_send = client_wallet.total_balance().await.expect("client balance");
    tracing::info!("Client balance after sending token: {client_bal_after_send} sat");

    // ─── Step 8: Metering Report Exchange ───
    tracing::info!("");
    tracing::info!("━━━ Step 8: Metering Report Exchange ━━━");
    tracing::info!("Per docs/design/core/tollgate-metering.md §2: Cumulative counter model");
    tracing::info!("Per docs/design/core/tollgate-pricing.md §3: Dual pricing formula");
    tracing::info!(
        "Formula: cost_scaled = elapsed_ms * price_per_second + delivered * price_per_unit"
    );
    tracing::info!("       balance = bootstrap_amount * scale - cost_scaled");

    let bootstrap_amount = 100u64;
    let scale = 1000u64;
    let pps = 10i64;
    let ppu = 1i64;

    let mut total_elapsed_ms: u64 = 0;
    let mut total_delivered: u64 = 0;
    let num_intervals = 3u64;

    for i in 1..=num_intervals {
        let interval_ms = 5000u64;
        let interval_units = 1000u64;
        total_elapsed_ms += interval_ms;
        total_delivered += interval_units;

        provider_adapter.set_metrics(PeerMetrics {
            elapsed_ms: total_elapsed_ms,
            delivered: total_delivered,
            received: total_delivered / 2,
        });

        let cost_scaled: i64 = (total_elapsed_ms as i64).saturating_mul(pps)
            + (total_delivered as i64).saturating_mul(ppu);
        let balance_scaled: i64 = (bootstrap_amount as i64)
            .saturating_mul(scale as i64)
            .saturating_sub(cost_scaled);
        tracing::info!("");
        tracing::info!(
            "  [Interval {i}] elapsed_ms={total_elapsed_ms}, delivered={total_delivered}"
        );
        tracing::info!(
            "  [Interval {i}] cost_scaled = {total_elapsed_ms}×{pps} + {total_delivered}×{ppu} = {cost_scaled}"
        );
        tracing::info!(
            "  [Interval {i}] balance_scaled = {bootstrap_amount}×{scale} - {cost_scaled} = {balance_scaled}"
        );

        let report = Message::MeteringReport(MeteringReport {
            msg_type: MessageType::MeteringReport as u8,
            elapsed_ms: total_elapsed_ms,
            delivered: total_delivered,
            received: total_delivered / 2,
            new_product_id: None,
            new_pricing: None,
        });

        let msgs = provider_session.handle_message(report).await;
        if msgs.is_empty() {
            tracing::info!("  [Interval {i}] ✓ Balance OK, session continues");
        } else {
            tracing::warn!("  [Interval {i}] Got {} response messages!", msgs.len());
            for msg in &msgs {
                tracing::warn!("  [Interval {i}] Response: {msg:?}");
            }
        }
    }

    assert!(
        provider_session.is_active(),
        "session should still be active after metering"
    );

    // ─── Step 9: Top-Up Test (another real Cashu token) ───
    tracing::info!("");
    tracing::info!("━━━ Step 9: Top-Up with Real Cashu Token ━━━");
    tracing::info!("Per docs/design/core/tollgate-bootstrap.md §4: Top-up flow");
    tracing::info!("[NUT-00] Creating top-up token for 50 sat");

    let topup_bytes = client_wallet
        .create_token(Amount(50), MINT_URL)
        .await
        .expect("create top-up token");
    tracing::info!("[NUT-00] Top-up token: {} bytes", topup_bytes.len());

    let topup_msg = Message::BootstrapToken(BootstrapToken {
        msg_type: MessageType::BootstrapToken as u8,
        token: topup_bytes,
    });

    let msgs = provider_session.handle_message(topup_msg).await;
    match &msgs[0] {
        Message::BootstrapAck(ack) => {
            tracing::info!("Top-up BootstrapAck: status={:?}", ack.status);
            assert_eq!(ack.status, BootstrapStatus::Accepted);
        }
        other => panic!("expected BootstrapAck for top-up, got {other:?}"),
    }

    let provider_final = provider_wallet
        .total_balance()
        .await
        .expect("provider final balance");
    tracing::info!("Provider balance after bootstrap + top-up: {provider_final} sat");
    // testnut.cashu.space charges input_fee_ppk=100 (1 sat per proof).
    // 100 sat token → 99 sat received, 50 sat token → 49 sat received = 148 total.
    // Assert >= 140 to account for variable number of proofs / fees.
    assert!(
        provider_final >= 140,
        "provider should have at least 140 sat from bootstrap + topup (after mint fees), got {provider_final}"
    );

    // ─── Step 10: Session Teardown ───
    tracing::info!("");
    tracing::info!("━━━ Step 10: Session Teardown ━━━");
    tracing::info!("Per docs/design/core/tollgate-protocol.md §3.5: Graceful disconnect");
    let disconnect = Message::Disconnect(Disconnect {
        msg_type: MessageType::Disconnect as u8,
        reason_code: ReasonCode::Other,
    });

    let msgs = provider_session.handle_message(disconnect).await;
    tracing::info!("Disconnect handled, {} response messages", msgs.len());
    match &msgs[0] {
        Message::Disconnect(_) => tracing::info!("Provider confirmed disconnect"),
        other => tracing::warn!("Unexpected disconnect response: {other:?}"),
    }
    tracing::info!(
        "Provider state after disconnect: {:?}",
        provider_session.state()
    );

    let access = provider_adapter.get_access_level(&client_pubkey().0);
    tracing::info!("Client access level after disconnect: {access:?}");
    assert_eq!(access, Some(AccessLevel::None));

    // ─── Final Report ───
    tracing::info!("");
    tracing::info!("╔══════════════════════════════════════════════════════════════╗");
    tracing::info!("║  Test Complete                                               ║");
    let pbal = provider_wallet
        .total_balance()
        .await
        .expect("provider balance");
    let cbal = client_wallet.total_balance().await.expect("client balance");
    tracing::info!("║  Provider wallet balance: {pbal} sat");
    tracing::info!("║  Client wallet balance:  {cbal} sat");
    tracing::info!("║  Tokens transferred: 150 sat (100 bootstrap + 50 top-up)    ║");
    tracing::info!("║  Metering intervals: {num_intervals}                                  ║");
    tracing::info!("╚══════════════════════════════════════════════════════════════╝");
}

// Helper trait to get product count from PriceSheet message
trait PriceSheetExt {
    fn products_count(&self) -> usize;
}

impl PriceSheetExt for Message {
    fn products_count(&self) -> usize {
        if let Message::PriceSheet(sheet) = self {
            sheet.products.len()
        } else {
            0
        }
    }
}
