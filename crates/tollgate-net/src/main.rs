//! `tollgate` — the TollGate node binary.
//!
//! First deployment target: plain IP network in **bootstrap-only mode**. Peers
//! pay with ordinary Cashu tokens and get metered access. Spilman channels and
//! FIPS integration are layered on later.

mod adapter;
mod client;
mod config;
mod control_server;
mod driver;
mod server;
mod wallet;

#[cfg(feature = "v1-compat")]
mod v1_compat;

#[cfg(feature = "openwrt")]
mod openwrt;

#[cfg(feature = "spilman")]
mod spilman;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "tollgate",
    version,
    about = "Sell metered network access with Cashu micropayments"
)]
struct Cli {
    /// Path to a config file. Searched in order if omitted:
    /// ./tollgate.yaml, ~/.config/tollgate/tollgate.yaml, /etc/tollgate/tollgate.yaml
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the node: listen for peers and sell metered access. (Default.)
    Serve {
        /// Override the listen address (default: 127.0.0.1:4747).
        #[arg(long)]
        listen: Option<String>,
    },
    /// Probe a peer: send our Announce and report the peer's identity.
    Connect {
        /// Peer HTTP origin, e.g. http://peer:4747
        #[arg(long)]
        peer: String,
    },
    /// Pay a peer a bootstrap token and report whether it was accepted.
    Pay {
        /// Peer HTTP origin, e.g. http://peer:4747
        #[arg(long)]
        peer: String,
        /// Mint URL to draw the token on, e.g. http://mint:3338
        #[arg(long)]
        mint: String,
        /// Token amount in sats.
        #[arg(long, default_value_t = 8)]
        amount: u64,
    },
    /// Pay a peer and stay online: poll for MeteringReports and auto-top-up
    /// before the balance runs out.
    Consume {
        /// Peer HTTP origin, e.g. http://peer:4747
        #[arg(long)]
        peer: String,
        /// Mint URL to draw tokens on, e.g. http://mint:3338
        #[arg(long)]
        mint: String,
        /// Initial token amount in sats.
        #[arg(long, default_value_t = 8)]
        amount: u64,
        /// Top-up amount in sats (also the low-balance watermark).
        #[arg(long, default_value_t = 8)]
        topup: u64,
        /// Seconds between polls.
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Stop after this many polls (0 = run until killed).
        #[arg(long, default_value_t = 0)]
        polls: u32,
        /// Fault injection (testing): under-report acknowledged received units by
        /// this percentage so the provider sees metering drift. Hidden; used by the
        /// drift integration suite.
        #[arg(long, default_value_t = 0, hide = true)]
        understate_received_pct: u8,
        /// Uplink interface (e.g. eth0) to meter for an independent receive-side
        /// count, instead of echoing the provider's delivered. Surfaces real
        /// transit drift. Only correct when this upstream owns the interface.
        #[arg(long)]
        meter_iface: Option<String>,
        /// Meter this upstream by its next-hop MAC (per-peer nftables counter) for
        /// an independent receive-side count that works even when upstreams share
        /// an interface. Requires NET_ADMIN.
        #[arg(long)]
        meter_upstream: bool,
    },
    #[cfg(feature = "v1-compat")]
    /// Pay an upstream Go v1 TollGate router using Cashu tokens (v1 compat mode).
    V1Consume {
        /// Gateway address (e.g. "192.168.1.1:2121").
        #[arg(long)]
        gateway: String,
        /// Mint URL to draw Cashu tokens on.
        #[arg(long)]
        mint: String,
        /// Initial token amount in sats.
        #[arg(long, default_value_t = 8)]
        amount: u64,
        /// Seconds between usage polls.
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Use Spilman payment channels instead of bootstrap tokens.
        #[cfg(feature = "spilman")]
        #[arg(long)]
        spilman: bool,
    },
    #[cfg(feature = "v1-compat")]
    /// Pay upstream Go v1 TollGate routers with auto-discovery (multi-gateway mode).
    V1Auto {
        /// Mint URL to draw Cashu tokens on.
        #[arg(long)]
        mint: String,
        /// Initial token amount in sats.
        #[arg(long, default_value_t = 8)]
        amount: u64,
        /// Seconds between usage polls.
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Gateway IPs to scan, comma-separated (e.g. "192.168.1.1,10.0.0.1").
        #[arg(long, value_delimiter = ',')]
        scan_interfaces: Option<Vec<String>>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Emit ANSI colors only to a real terminal. Under docker/journald the output
    // is piped, where escape codes corrupt field text (and break log greps).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let cli = Cli::parse();
    let cfg = config::Config::load(cli.config.as_deref())?;
    let identity = Arc::new(config::Identity::load_or_generate(&cfg)?);

    match cli.command.unwrap_or(Command::Serve { listen: None }) {
        Command::Serve { listen } => serve(cfg, identity, listen).await,
        Command::Connect { peer } => connect(&cfg, &identity, &peer).await,
        Command::Pay { peer, mint, amount } => pay(&cfg, &identity, &peer, &mint, amount).await,
        Command::Consume {
            peer,
            mint,
            amount,
            topup,
            interval,
            polls,
            understate_received_pct,
            meter_iface,
            meter_upstream,
        } => {
            let opts = client::ConsumeOpts {
                amount_sat: amount,
                topup_sat: topup,
                interval: std::time::Duration::from_secs(interval),
                max_polls: (polls > 0).then_some(polls),
                understate_received_pct,
                meter_iface,
                meter_upstream,
            };
            consume(&cfg, &identity, &peer, &mint, opts).await
        }
        #[cfg(feature = "v1-compat")]
        Command::V1Consume {
            gateway, mint, amount, interval,
            #[cfg(feature = "spilman")]
                spilman,
        } => {
            #[cfg(feature = "spilman")]
            if spilman {
                v1_consume_spilman(&gateway, &mint, amount, interval).await
            } else {
                v1_consume(&gateway, &mint, amount, interval).await
            }
            #[cfg(not(feature = "spilman"))]
            v1_consume(&gateway, &mint, amount, interval).await
        }
        #[cfg(feature = "v1-compat")]
        Command::V1Auto { mint, amount, interval, scan_interfaces } => {
            v1_auto(&mint, amount, interval, scan_interfaces).await
        }
    }
}

async fn serve(
    mut cfg: config::Config,
    identity: Arc<config::Identity>,
    listen: Option<String>,
) -> anyhow::Result<()> {
    if let Some(listen) = listen {
        cfg.listen = listen;
    }
    tracing::info!(pubkey = %identity.pubkey_hex(), listen = %cfg.listen, "starting tollgate node");

    let wallet = wallet::BootstrapWallet::new(cfg.mints.clone());
    let adapter = adapter::IpAdapter::new();
    if let Err(e) = adapter.init(cfg.firewall.installs_forward_chain()) {
        tracing::warn!(err = %e, "firewall init failed; access may not be enforced (need root?)");
    }

    // v1: one price for all peers, taken from the first configured product.
    let price = cfg
        .products
        .first()
        .map(|p| tollgate_core::Price {
            per_second: p.price_per_second,
            per_unit: p.price_per_unit,
        })
        .unwrap_or_default();

    let driver = driver::Driver::new(
        wallet,
        adapter,
        identity.clone(),
        price,
        cfg.unit.clone(),
        cfg.price_sheet().encode(),
    );
    driver.spawn_metering(std::time::Duration::from_secs(
        cfg.metering_interval_secs.max(1),
    ));
    // Buy from configured upstreams: pay + auto-top-up each, tracking them as
    // upstream peers (the mesh's inbound direction).
    driver.spawn_upstreams(cfg.upstreams.clone(), identity);
    // Reap peers that have gone silent. The HTTP-polling transport has no socket
    // close to observe, so idle-timeout is the disconnect signal; Active peers are
    // kept regardless (they hold paid balance and may consume without polling).
    driver.spawn_reaper(
        std::time::Duration::from_secs(120),
        std::time::Duration::from_secs(30),
    );

    // Serve the FIPS-style control socket so `tolltop` / status tooling can read
    // live peer state. Best-effort: a bind failure is logged, not fatal.
    {
        let driver = driver.clone();
        let socket = cfg.control_socket.clone();
        tokio::spawn(async move {
            if let Err(e) = control_server::serve(&socket, driver).await {
                tracing::warn!(err = %e, socket = %socket.display(), "control socket stopped");
            }
        });
    }

    #[cfg(feature = "v1-compat")]
    let v1_wallet = match build_v1_wallet(&cfg.v1_compat).await {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::error!(err = %e, "v1 wallet construction failed; LN invoice endpoints will return 503");
            None
        }
    };

    #[cfg(feature = "v1-compat")]
    {
        server::serve(&cfg.listen, driver, &cfg, v1_wallet).await
    }
    #[cfg(not(feature = "v1-compat"))]
    {
        server::serve(&cfg.listen, driver, &cfg).await
    }
}

#[cfg(feature = "v1-compat")]
async fn build_v1_wallet(
    v1: &crate::config::V1CompatConfig,
) -> anyhow::Result<std::sync::Arc<crate::v1_compat::wallet::CdkWallet>> {
    use crate::v1_compat::wallet::CdkWallet;

    let seed_hex = v1
        .wallet_seed
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("v1_compat.wallet_seed not set"))?;
    let seed_bytes = hex::decode(seed_hex.trim())
        .map_err(|e| anyhow::anyhow!("invalid wallet_seed hex: {e}"))?;
    let seed: [u8; 64] = seed_bytes
        .as_slice()
        .try_into()
        .map_err(|_| {
            anyhow::anyhow!(
                "wallet_seed must decode to exactly 64 bytes, got {}",
                seed_bytes.len()
            )
        })?;

    let mint_url = v1
        .accepted_mints
        .first()
        .map(|m| m.url.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("http://localhost:3338");

    let wallet = CdkWallet::new(mint_url, seed)
        .await
        .map_err(|e| anyhow::anyhow!("wallet init: {e}"))?;
    Ok(std::sync::Arc::new(wallet))
}

async fn connect(
    cfg: &config::Config,
    identity: &config::Identity,
    peer: &str,
) -> anyhow::Result<()> {
    tracing::info!(pubkey = %identity.pubkey_hex(), %peer, "probing peer");
    let detected = client::detect(peer, identity, &cfg.unit).await?;
    tracing::info!(
        peer_pubkey = %detected.pubkey_hex,
        peer_unit = %detected.unit,
        peer_version = detected.version,
        "detected peer"
    );
    // Machine-readable lines for test harnesses to grep.
    println!(
        "DETECTED peer={} unit={} version={}",
        detected.pubkey_hex, detected.unit, detected.version
    );
    if let Some(sheet) = detected.price_sheet.as_ref() {
        println!("{}", format_price_sheet(sheet));
    }
    Ok(())
}

/// A one-line, greppable summary of a peer's advertised PriceSheet — the first
/// product's first mint option, or a `mints=0` form when a product lists none.
fn format_price_sheet(sheet: &tollgate_protocol::PriceSheet) -> String {
    let products = sheet.products.len();
    match sheet.products.first().and_then(|p| p.mints.first()) {
        Some(mint) => format!(
            "PRICESHEET products={} mints={} per_second={} per_unit={} mint_unit={}",
            products,
            sheet.products.first().map_or(0, |p| p.mints.len()),
            mint.price_per_second,
            mint.price_per_unit,
            mint.mint_unit
        ),
        None => format!("PRICESHEET products={products} mints=0"),
    }
}

async fn pay(
    cfg: &config::Config,
    identity: &config::Identity,
    peer: &str,
    mint: &str,
    amount: u64,
) -> anyhow::Result<()> {
    tracing::info!(pubkey = %identity.pubkey_hex(), %peer, %mint, amount, "paying bootstrap token");
    let paid = client::pay(peer, identity, &cfg.unit, mint, amount).await?;
    tracing::info!(
        peer_pubkey = %paid.peer_pubkey_hex,
        accepted = paid.accepted,
        reason = ?paid.reason,
        "bootstrap result"
    );
    // Machine-readable line for test harnesses to grep.
    println!(
        "PAID peer={} accepted={} reason={}",
        paid.peer_pubkey_hex,
        paid.accepted,
        paid.reason.as_deref().unwrap_or("-")
    );
    if let Some(sheet) = paid.price_sheet.as_ref() {
        println!("{}", format_price_sheet(sheet));
    }
    if !paid.accepted {
        anyhow::bail!(
            "bootstrap rejected: {}",
            paid.reason.as_deref().unwrap_or("unknown")
        );
    }
    Ok(())
}

async fn consume(
    cfg: &config::Config,
    identity: &config::Identity,
    peer: &str,
    mint: &str,
    opts: client::ConsumeOpts,
) -> anyhow::Result<()> {
    tracing::info!(
        pubkey = %identity.pubkey_hex(),
        %peer, %mint,
        amount = opts.amount_sat,
        topup = opts.topup_sat,
        "consuming: pay then auto-top-up"
    );
    client::consume(peer, identity, &cfg.unit, mint, opts).await
}

#[cfg(feature = "v1-compat")]
async fn v1_consume(
    gateway: &str,
    mint: &str,
    amount: u64,
    interval: u64,
) -> anyhow::Result<()> {
    use v1_compat::client::V1ClientConfig;
    use v1_compat::wallet::CdkWallet;

    tracing::info!(%gateway, %mint, amount, interval, "v1 consume: paying upstream Go router");

    let wallet = CdkWallet::new(mint, [0u8; 64]).await
        .map_err(|e| anyhow::anyhow!("wallet init failed: {e}"))?;

    let base_url = if gateway.starts_with("http") {
        gateway.to_string()
    } else {
        format!("http://{gateway}")
    };

    let config = V1ClientConfig {
        gateway_ip: base_url.clone(),
        mac_address: "00:00:00:00:00:00".to_string(),
        our_mint_urls: vec![mint.to_string()],
        unit: "sat".to_string(),
        max_price_per_ms: 0.0,
        max_price_per_byte: 0.0,
        preferred_allotment: amount * 60_000,
        poll_interval_secs: interval,
        renewal_threshold: 0.2,
    };

    tracing::info!(gateway = %base_url, "v1 client configured; starting consume loop");

    let mut client = v1_compat::client::V1Client::new(config);
    client.run(std::sync::Arc::new(wallet)).await
        .map_err(|e| anyhow::anyhow!("v1 client error: {e}"))?;

    Ok(())
}

#[cfg(all(feature = "v1-compat", feature = "spilman"))]
async fn v1_consume_spilman(
    gateway: &str,
    mint: &str,
    amount: u64,
    interval: u64,
) -> anyhow::Result<()> {
    use cashu::nuts::SecretKey;
    use v1_compat::http_client::TollGateHttpClient;
    use v1_compat::wallet::CdkWallet;
    use spilman::service::{ReqwestNetworking, SpilmanService};
    use spilman::wallet::fetch_active_keyset_info;

    tracing::info!(%gateway, %mint, amount, "v1 consume (spilman mode): opening payment channel");

    let base_url = if gateway.starts_with("http") { gateway.to_string() } else { format!("http://{gateway}") };

    tracing::info!("Fetching advertisement from gateway...");
    let http = TollGateHttpClient::new_with_base_url(&base_url);
    let ad = http.fetch_advertisement().await
        .map_err(|e| anyhow::anyhow!("advertisement fetch failed: {e}"))?;
    let gateway_pubkey = ad.pubkey().to_hex();
    tracing::info!(gateway_pubkey = %gateway_pubkey, "Got gateway pubkey from advertisement");

    tracing::info!("Fetching keyset info from mint...");
    let (mint_url, keyset_info) = fetch_active_keyset_info(mint).await
        .map_err(|e| anyhow::anyhow!("keyset fetch failed: {e}"))?;
    let keyset_info_json = serde_json::to_string(&keyset_info)
        .map_err(|e| anyhow::anyhow!("keyset serialization failed: {e}"))?;
    tracing::info!(%mint_url, "Got keyset info");

    tracing::info!("Initializing SpilmanService...");
    let sender_secret = SecretKey::generate();
    let service = SpilmanService::new(mint, sender_secret);
    let net = ReqwestNetworking::new();
    tracing::info!(sender = %service.sender_pubkey(), "SpilmanService initialized");

    tracing::info!("Creating CdkWallet to fund channel...");
    let wallet = CdkWallet::new(mint, [0u8; 64]).await
        .map_err(|e| anyhow::anyhow!("wallet init failed: {e}"))?;
    let token = wallet.create_token(amount, mint).await
        .map_err(|e| anyhow::anyhow!("token creation failed: {e}"))?;
    let token_str = String::from_utf8(token)
        .map_err(|e| anyhow::anyhow!("token not UTF-8: {e}"))?;
    tracing::info!("Funding token created");

    tracing::info!("Opening Spilman channel...");
    let channel = service.open_channel(
        &token_str,
        &gateway_pubkey,
        3600,
        &keyset_info_json,
        amount,
        &net,
    ).await.map_err(|e| anyhow::anyhow!("channel open failed: {e}"))?;

    let channel_id = channel.channel_id.clone();
    tracing::info!(channel_id = %channel_id, "Channel opened successfully!");

    tracing::info!("Starting payment loop (interval={}s)...", interval);
    let mut balance = 1u64;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

        match service.create_payment(&channel_id, balance) {
            Ok(payment) => {
                tracing::info!(
                    channel_id = %channel_id,
                    balance,
                    payment_balance = payment.balance,
                    "Payment created (signed)"
                );
                balance += 1;
            }
            Err(e) => {
                tracing::warn!(err = %e, "Payment creation failed");
                break;
            }
        }

        if balance > amount * 100 {
            tracing::info!("Balance cap reached, requesting cooperative close...");
            match service.request_cooperative_close(&channel_id, balance - 1) {
                Ok(_) => tracing::info!("Cooperative close requested"),
                Err(e) => tracing::warn!(err = %e, "Close request failed"),
            }
            break;
        }
    }

    tracing::info!("Spilman consume session ended");
    Ok(())
}

#[cfg(feature = "v1-compat")]
async fn v1_auto(
    mint: &str,
    amount: u64,
    interval: u64,
    scan_interfaces: Option<Vec<String>>,
) -> anyhow::Result<()> {
    use v1_compat::client::V1ClientConfig;
    use v1_compat::crowsnest::{Crowsnest, CrowsnestConfig, GatewayHandler};
    use v1_compat::session_manager::{SessionManager, SessionManagerConfig};
    use v1_compat::usage_tracker::UsageTrackerConfig;
    use v1_compat::wallet::CdkWallet;

    tracing::info!(%mint, amount, interval, "v1 auto: multi-gateway discovery mode");

    let wallet = Arc::new(
        CdkWallet::new(mint, [0u8; 64])
            .await
            .map_err(|e| anyhow::anyhow!("wallet init failed: {e}"))?,
    );

    let client_config = V1ClientConfig {
        gateway_ip: String::new(),
        mac_address: "00:00:00:00:00:00".to_string(),
        our_mint_urls: vec![mint.to_string()],
        unit: "sat".to_string(),
        max_price_per_ms: 0.0,
        max_price_per_byte: 0.0,
        preferred_allotment: amount * 60_000,
        poll_interval_secs: interval,
        renewal_threshold: 0.2,
    };

    let tracker_config = UsageTrackerConfig {
        poll_interval: std::time::Duration::from_secs(interval),
        renewal_threshold: 0.8,
    };

    let session_manager = Arc::new(SessionManager::new(
        SessionManagerConfig {
            client_config,
            tracker_config,
        },
        wallet,
    ));

    let gateway_ips = scan_interfaces.unwrap_or_else(|| vec!["192.168.1.1".to_string()]);
    let crowsnest_config = CrowsnestConfig {
        gateway_ips,
        scan_interval: std::time::Duration::from_secs(30),
        ..Default::default()
    };

    let handler: Arc<dyn GatewayHandler> = session_manager.clone();
    let crowsnest = Crowsnest::new(crowsnest_config, handler);
    let _crowsnest_handle = crowsnest.spawn();

    tracing::info!("Crowsnest spawned; session manager running");

    session_manager
        .run()
        .await
        .map_err(|e| anyhow::anyhow!("session manager error: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tollgate_protocol::{MintPrice, PriceSheet, ProductOffer};

    #[test]
    fn format_price_sheet_summarizes_first_mint_option() {
        let prices = vec![MintPrice {
            mint_url: "http://m".to_string(),
            price_per_second: 2,
            price_per_unit: 7,
            mint_unit: "sat".to_string(),
        }];
        let sheet = PriceSheet::new(vec![ProductOffer::new(1000, &prices, vec![])], 5000, 60000);
        let line = format_price_sheet(&sheet);
        assert!(line.contains("products=1"), "{line}");
        assert!(line.contains("mints=1"), "{line}");
        assert!(line.contains("per_second=2"), "{line}");
        assert!(line.contains("per_unit=7"), "{line}");
        assert!(line.contains("mint_unit=sat"), "{line}");
    }

    #[test]
    fn format_price_sheet_handles_product_without_mints() {
        // A product configured with no accepted mints (the detect-only case).
        let sheet = PriceSheet::new(vec![ProductOffer::new(1000, &[], vec![])], 5000, 60000);
        assert_eq!(format_price_sheet(&sheet), "PRICESHEET products=1 mints=0");
    }
}
