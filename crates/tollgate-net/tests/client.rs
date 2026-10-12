//! A buyer session run through the library, against a real gateway.
//!
//! The gateway is a whole node with its own mint and real Spilman channels;
//! the session is [`tollgate_net::client::run`], funded from a wallet of its
//! own that mints at the gateway, as `proxyd` funds a phone's session out of
//! what the phone paid. What is asserted is what the phone would feel: the
//! gateway shapes it to exactly the rate the session asked for, and to a new
//! one when the rate changes.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tollgate_net::Identity;
use tollgate_net::channel::{SpilmanChannels, SpilmanConfig};
use tollgate_net::client::{self, Buying, ClientSpec, Moved};
use tollgate_net::config::File;
use tollgate_net::enforcer::{Enforcer, Loopback};
use tollgate_net::mint::{self, IssueLimit, MintConfig};
use tollgate_net::node::Node;
use tollgate_net::wallet::{Wallet, WalletFunding};
use tollgate_protocol::PubKey;

fn free() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
}

fn temp_dir() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "tollgate-client-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("a temporary directory");
    path
}

async fn wait_for(label: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {label}");
}

/// A gateway: a node that sells, with its own mint giving vouchers away so a
/// session can be funded. `extra` is added to its config.
struct Gateway {
    enforcer: Arc<Loopback>,
    pubkey: String,
    endpoint: String,
    mint_url: String,
}

async fn gateway(dir: &std::path::Path, extra: &str) -> Gateway {
    // The gateway's mint, giving vouchers away so the session can be funded.
    let (public, private) = (free(), free());
    let mint_url = format!("http://{public}");
    let gateway_mint = Arc::new(
        mint::build(&MintConfig {
            url: mint_url.clone(),
            unit: "byte".into(),
            seed: vec![21; 32],
            file: dir.join("mint.sqlite"),
            max_amount: 1 << 34,
        })
        .await
        .expect("the gateway's mint"),
    );
    tokio::spawn(mint::serve(
        gateway_mint,
        mint::Listeners {
            public,
            private,
            auto_accept: true,
            issue_limit: IssueLimit::default(),
        },
        Arc::default(),
        std::future::pending(),
    ));

    // The gateway: a node that sells, with a generous allowance of zero so
    // the only rate the session gets is the one it bought.
    let gateway_id = Identity::generate();
    let control = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the gateway");
    let endpoint = control.local_addr().expect("address").to_string();
    let file: File = serde_yaml::from_str(&format!(
        "identity: {{ secret_key: \"{}\" }}\nmint: {{ url: \"{mint_url}\" }}\n{extra}",
        gateway_id.secret_hex()
    ))
    .expect("gateway config");
    let config = file.resolve().expect("resolve");
    let gateway_wallet = Wallet::open(dir.join("gateway-wallet.sqlite"), [22; 64], "byte")
        .await
        .expect("wallet");
    let channels = Arc::new(
        SpilmanChannels::new(SpilmanConfig {
            mint_url: config.mint_url.clone(),
            mint_local: config.mint_local.clone(),
            unit: config.policy.unit.clone(),
            accepted_mints: config.policy.accepted_mints.clone(),
            burned_mints: Vec::new(),
            secret_key_hex: config.identity.secret_hex(),
            funding: Arc::new(WalletFunding::new(gateway_wallet)),
            ttl_seconds: config.channel_ttl_seconds,
        })
        .expect("gateway channels"),
    );
    let gateway = Arc::new(Loopback::new());
    let node = Node::new(&config, channels, gateway.clone());
    tokio::spawn(node.run_on(control, config, std::future::pending()));

    Gateway {
        enforcer: gateway,
        pubkey: hex::encode(gateway_id.pubkey().0),
        endpoint,
        mint_url,
    }
}

/// A wallet of the session's own that mints at the gateway, as proxyd funds a
/// phone's session out of what the phone paid.
async fn session_funding(dir: &std::path::Path) -> Arc<WalletFunding> {
    let wallet = Wallet::open(dir.join("session-wallet.sqlite"), [23; 64], "byte")
        .await
        .expect("wallet");
    Arc::new(WalletFunding::new(wallet))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_buys_the_rate_it_is_given_and_follows_a_change() {
    let dir = temp_dir();
    let Gateway {
        enforcer: gateway,
        pubkey,
        endpoint,
        mint_url,
    } = gateway(&dir, "").await;

    // The session, as proxyd runs one for a phone.
    let session_id = Identity::generate();
    let (rate_tx, rate_rx) = tokio::sync::watch::channel(1_000_000u64);
    tokio::spawn(client::run(
        ClientSpec {
            secret_hex: session_id.secret_hex(),
            gateway: pubkey,
            endpoint,
            gateway_mint: mint_url,
            channel_capacity: 1 << 28,
            buying: tokio::sync::watch::channel(Buying::AtSpeed).1,
            moved: None,
        },
        session_funding(&dir).await,
        None,
        rate_rx,
        std::future::pending(),
    ));

    let session: PubKey = session_id.pubkey();
    let probe = gateway.clone();
    wait_for(
        "the gateway to shape the session to what it bought",
        move || probe.shaping_rate(session) == 1_000_000,
    )
    .await;

    rate_tx.send(3_000_000).expect("change the rate");
    let probe = gateway.clone();
    wait_for("the gateway to follow the new rate", move || {
        probe.shaping_rate(session) == 3_000_000
    })
    .await;

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pay_per_use_session_buys_back_what_was_moved_and_moves_to_a_speed_in_place() {
    let dir = temp_dir();
    // The gateway caps a payer that reserves nothing at 2 MB/s.
    let Gateway {
        enforcer: gateway,
        pubkey,
        endpoint,
        mint_url,
    } = gateway(&dir, "burst: { unreserved_rate: 2000000 }\n").await;

    let session_id = Identity::generate();
    let (rate_tx, rate_rx) = tokio::sync::watch::channel(2_000_000u64);
    let (moved_tx, moved_rx) = tokio::sync::watch::channel(Moved::default());
    let pay_per_use = Buying::PayPerUse {
        budget: 10_000_000,
        renew_lead_ms: 2_000,
    };
    let (buying_tx, buying_rx) = tokio::sync::watch::channel(pay_per_use);
    tokio::spawn(client::run(
        ClientSpec {
            secret_hex: session_id.secret_hex(),
            gateway: pubkey,
            endpoint,
            gateway_mint: mint_url,
            channel_capacity: 1 << 28,
            buying: buying_rx,
            moved: Some(moved_rx),
        },
        session_funding(&dir).await,
        None,
        rate_rx,
        std::future::pending(),
    ));

    // Reserving nothing, it is carried at the gateway's cap, not a rate of
    // its own.
    let session: PubKey = session_id.pubkey();
    let probe = gateway.clone();
    wait_for("the gateway to carry the session at its cap", move || {
        probe.shaping_rate(session) == 2_000_000
    })
    .await;

    // The device downloads 9.9 MB through the gateway. The gateway draws it,
    // and with a tenth of a megabyte left it slows the device so it cannot
    // move more in a tick than is left.
    gateway.record_delivered(session, 9_900_000);
    let probe = gateway.clone();
    wait_for("the gateway to clip what is nearly spent", move || {
        probe.shaping_rate(session) < 2_000_000
    })
    .await;

    // Once the session hears what moved, it buys that back, and the device is
    // carried at the cap again.
    moved_tx
        .send(Moved {
            to_device: 9_900_000,
            from_device: 0,
        })
        .expect("report what moved");
    let probe = gateway.clone();
    wait_for("the session to buy back what was moved", move || {
        probe.shaping_rate(session) == 2_000_000
    })
    .await;

    // Moved to time at a speed in place, it reserves the rate at once, and
    // back to pay per use it ends the reservation at once.
    rate_tx.send(1_000_000).expect("the speed");
    buying_tx.send(Buying::AtSpeed).expect("move to a speed");
    let probe = gateway.clone();
    wait_for("the session to reserve the speed", move || {
        probe.shaping_rate(session) == 1_000_000
    })
    .await;
    rate_tx.send(2_000_000).expect("the cap");
    buying_tx.send(pay_per_use).expect("back to pay per use");
    let probe = gateway.clone();
    wait_for("the session to end its reservation", move || {
        probe.shaping_rate(session) == 2_000_000
    })
    .await;

    let _ = std::fs::remove_dir_all(dir);
}
