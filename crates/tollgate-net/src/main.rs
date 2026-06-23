//! `tollgate` — the TollGate node binary.
//!
//! First deployment target: plain IP network in **bootstrap-only mode**. Peers
//! pay with ordinary Cashu tokens and get metered access. Spilman channels and
//! FIPS integration are layered on later.

mod adapter;
mod client;
mod config;
mod driver;
mod server;
#[cfg(feature = "v1-compat")]
mod v1;
mod wallet;

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
        /// Peer HTTP origin, e.g. http://gateway:4747
        #[arg(long)]
        peer: String,
    },
    /// Pay a peer a bootstrap token and report whether it was accepted.
    Pay {
        /// Peer HTTP origin, e.g. http://gateway:4747
        #[arg(long)]
        peer: String,
        /// Mint URL to draw the token on, e.g. http://mint:3338
        #[arg(long)]
        mint: String,
        /// Token amount in sats.
        #[arg(long, default_value_t = 8)]
        amount: u64,
    },
    /// Run the v1 HTTP/JSON compatibility server (Go v1 API on port 2121).
    #[cfg(feature = "v1-compat")]
    V1Serve {
        /// Override the listen address (default: 127.0.0.1:2121).
        #[arg(long)]
        listen: Option<String>,
        /// Override the mint URL (default: taken from config or
        /// <https://testnut.cashu.exchange>).
        #[arg(long)]
        mint: Option<String>,
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
        #[cfg(feature = "v1-compat")]
        Command::V1Serve { listen, mint } => v1_serve(cfg, identity, listen, mint).await,
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

    let driver = driver::Driver::new(wallet, adapter, identity, price, cfg.unit.clone());
    driver.spawn_metering(std::time::Duration::from_secs(5));

    server::serve(&cfg.listen, driver).await
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
    // Machine-readable line for test harnesses to grep.
    println!(
        "DETECTED peer={} unit={} version={}",
        detected.pubkey_hex, detected.unit, detected.version
    );
    Ok(())
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
    if !paid.accepted {
        anyhow::bail!(
            "bootstrap rejected: {}",
            paid.reason.as_deref().unwrap_or("unknown")
        );
    }
    Ok(())
}

#[cfg(feature = "v1-compat")]
async fn v1_serve(
    cfg: config::Config,
    identity: Arc<config::Identity>,
    listen: Option<String>,
    mint_override: Option<String>,
) -> anyhow::Result<()> {
    let listen = listen.unwrap_or_else(|| "127.0.0.1:2121".to_string());

    let mints: Vec<String> = mint_override.clone().map(|m| vec![m]).unwrap_or_else(|| {
        if cfg.mints.is_empty() {
            vec!["https://testnut.cashu.exchange".to_string()]
        } else {
            cfg.mints.clone()
        }
    });

    let wallet = wallet::BootstrapWallet::new(mints.clone());
    let adapter = adapter::IpAdapter::new();
    if let Err(e) = adapter.init(cfg.firewall.installs_forward_chain()) {
        tracing::warn!(err = %e, "firewall init failed; access may not be enforced (need root?)");
    }

    let primary_mint = mints.first().cloned().unwrap_or_default();
    let v1_config = v1::V1Config {
        metric: "milliseconds".to_string(),
        step_size: 60_000,
        price_per_step: 1,
        unit: "sat".to_string(),
        mint_url: primary_mint,
        min_steps: 1,
        tips: (1..=10).map(|i| i.to_string()).collect(),
    };

    tracing::info!(
        pubkey = %identity.pubkey_hex(),
        %listen,
        mints = ?mints,
        "starting v1 HTTP/JSON server",
    );

    let server = v1::V1Server::new(&identity, wallet, adapter, v1_config)?;
    server.serve(&listen).await
}
