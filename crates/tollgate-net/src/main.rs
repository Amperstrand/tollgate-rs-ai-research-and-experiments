use std::sync::Arc;

use clap::{Parser, Subcommand};
use tollgate_net::{cdk_wallet, client, mock, server, v1};

#[cfg(feature = "spilman")]
use {cashu::nuts::SecretKey, tollgate_net::spilman_service::SpilmanService};

#[derive(Parser)]
#[command(name = "tollgate-net", about = "TollGate v2 network node")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum WalletType {
    Mock,
    Cdk,
    #[cfg(feature = "spilman")]
    Spilman,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as a provider (sells network access)
    Provider {
        /// Port to listen on
        #[arg(long, default_value = "3001")]
        port: u16,
        /// Wallet backend: "mock" (default) or "cdk"
        #[arg(long, default_value = "mock")]
        wallet: WalletType,
        /// Mint URL (only used with --wallet cdk)
        #[arg(long, default_value = "https://testnut.cashu.exchange")]
        mint_url: String,
    },
    /// Run as a client (buys network access)
    Client {
        /// Provider URL to connect to
        #[arg(long, default_value = "http://localhost:3001")]
        peer: String,
        /// Number of metering intervals to run
        #[arg(long, default_value = "20")]
        intervals: u32,
        /// Interval duration in seconds
        #[arg(long, default_value = "1")]
        interval_secs: u64,
        /// Wallet backend: "mock" (default) or "cdk"
        #[arg(long, default_value = "mock")]
        wallet: WalletType,
        /// Mint URL (only used with --wallet cdk)
        #[arg(long, default_value = "https://testnut.cashu.exchange")]
        mint_url: String,
        /// Seller's Spilman receiver public key (hex, required for --wallet spilman)
        #[arg(long)]
        receiver_pubkey: Option<String>,
        /// Skip cooperative close and disconnect (for unilateral close testing)
        #[arg(long, default_value = "false")]
        no_close: bool,
    },
    /// Run as a v1 server (accepts Cashu token payments for network access)
    V1Server {
        /// Port to listen on (default: 2121)
        #[arg(long, default_value = "2121")]
        port: u16,
        /// Metric type: "milliseconds" or "bytes"
        #[arg(long, default_value = "milliseconds")]
        metric: String,
        /// Step size (e.g. 60000 for 1 minute when metric is milliseconds)
        #[arg(long, default_value = "60000")]
        step_size: u64,
        /// Mint URL to accept
        #[arg(long, default_value = "https://testnut.cashu.exchange")]
        mint_url: String,
        /// Price per step in sats
        #[arg(long, default_value = "1")]
        price_per_step: u64,
        /// Minimum purchase steps
        #[arg(long, default_value = "1")]
        min_steps: u64,
        /// Wallet backend
        #[arg(long, default_value = "mock")]
        wallet: WalletType,
        /// Path to JSON config file (overrides CLI args for metric/step_size/mints)
        #[arg(long)]
        config: Option<String>,
    },
    /// Run as a v1 client (pays upstream TollGate routers via TIP-03)
    V1Client {
        /// Gateway IP of the upstream TollGate
        #[arg(long)]
        gateway: String,
        /// MAC address of our interface (device-identifier)
        #[arg(long, default_value = "00:00:00:00:00:00")]
        mac: String,
        /// Mint URL (must match an accepted mint on the upstream TollGate)
        #[arg(long, default_value = "https://testnut.cashu.exchange")]
        mint_url: String,
        /// Currency unit
        #[arg(long, default_value = "sat")]
        unit: String,
        /// Preferred allotment (milliseconds for time, bytes for data)
        #[arg(long, default_value = "60000")]
        preferred_allotment: u64,
        /// Usage polling interval in seconds
        #[arg(long, default_value = "1")]
        poll_interval: u64,
        /// Renewal threshold (0.0–1.0, renew when usage reaches this fraction)
        #[arg(long, default_value = "0.8")]
        renewal_threshold: f64,
        /// Max price per millisecond (0 = no limit)
        #[arg(long, default_value = "0.01")]
        max_price_per_ms: f64,
        /// Max price per byte (0 = no limit)
        #[arg(long, default_value = "0.0001")]
        max_price_per_byte: f64,
    },
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() {
    v1::server::init_logging("info");

    let cli = Cli::parse();
    match cli.command {
        Commands::Provider {
            port,
            wallet: wt,
            mint_url,
        } => match wt {
            WalletType::Mock => {
                let wallet = Arc::new(mock::MockWallet::new(0));
                server::run(port, wallet).await;
            }
            WalletType::Cdk => {
                let wallet = Arc::new(
                    cdk_wallet::CdkWallet::new(&mint_url, [1u8; 64])
                        .await
                        .expect("failed to create CDK wallet"),
                );
                server::run(port, wallet).await;
            }
            #[cfg(feature = "spilman")]
            WalletType::Spilman => {
                let wallet = Arc::new(
                    cdk_wallet::CdkWallet::new(&mint_url, [1u8; 64])
                        .await
                        .expect("failed to create CDK wallet"),
                );
                let receiver_secret = SecretKey::generate();
                server::run_spilman(port, wallet, receiver_secret, &mint_url).await;
            }
        },
        Commands::Client {
            peer,
            intervals,
            interval_secs,
            wallet: wt,
            mint_url,
            receiver_pubkey,
            no_close,
        } => match wt {
            WalletType::Mock => {
                let _ = (&receiver_pubkey, &no_close);
                client::run_mock(&peer, intervals, interval_secs, 200).await;
            }
            WalletType::Cdk => {
                let _ = (&receiver_pubkey, &no_close);
                let wallet = Arc::new(
                    cdk_wallet::CdkWallet::new(&mint_url, [2u8; 64])
                        .await
                        .expect("failed to create CDK wallet"),
                );
                client::run_cdk(&peer, intervals, interval_secs, wallet, &mint_url).await;
            }
            #[cfg(feature = "spilman")]
            WalletType::Spilman => {
                let wallet = Arc::new(
                    cdk_wallet::CdkWallet::new(&mint_url, [2u8; 64])
                        .await
                        .expect("failed to create CDK wallet"),
                );
                let sender_secret = SecretKey::generate();
                #[allow(clippy::arc_with_non_send_sync)]
                let spilman = Arc::new(SpilmanService::new(&mint_url, sender_secret));
                let receiver_pk = receiver_pubkey.as_deref().unwrap_or_else(|| {
                    eprintln!("ERROR: --receiver-pubkey is required for --wallet spilman");
                    eprintln!(
                        "Start the provider first; it prints its receiver pubkey on startup."
                    );
                    std::process::exit(1);
                });
                client::run_spilman(
                    &peer,
                    intervals,
                    interval_secs,
                    wallet,
                    spilman,
                    receiver_pk,
                    &mint_url,
                    no_close,
                )
                .await;
            }
        },
        Commands::V1Server {
            port,
            metric,
            step_size,
            mint_url,
            price_per_step,
            min_steps,
            wallet: wt,
            config: config_path,
        } => {
            use std::time::Duration;
            use v1::server::payout::{PayoutConfig, PayoutTarget};

            let nostr_keys = nostr::prelude::Keys::generate();
            let server_config = if let Some(path) = config_path {
                v1::server::ServerConfig::load_from_file(&path).unwrap_or_else(|e| {
                    eprintln!("Failed to load config from {path}: {e}");
                    std::process::exit(1);
                })
            } else {
                let mut sc = v1::server::ServerConfig {
                    metric,
                    step_size,
                    ..v1::server::ServerConfig::default()
                };
                sc.accepted_mints = vec![v1::server::config::MintConfig {
                    url: mint_url.clone(),
                    min_balance: 64,
                    balance_tolerance_percent: 10,
                    payout_interval_seconds: 60,
                    min_payout_amount: 128,
                    price_per_step,
                    price_unit: "sat".to_owned(),
                    purchase_min_steps: min_steps,
                }];
                sc
            };
            let config = server_config.to_server_config(nostr_keys, port);
            let server = v1::server::V1Server::new(config);
            match wt {
                WalletType::Mock => {
                    let wallet = Arc::new(mock::MockWallet::new(0));
                    server.run(wallet).await;
                }
                WalletType::Cdk => {
                    let wallet = Arc::new(
                        cdk_wallet::CdkWallet::new(&mint_url, [4u8; 64])
                            .await
                            .expect("failed to create CDK wallet"),
                    );
                    let mint_cfg = &server_config.accepted_mints[0];
                    let profit_share = server_config.profit_share.clone();
                    let payout_cfg = PayoutConfig {
                        min_balance: mint_cfg.min_balance,
                        min_payout_amount: mint_cfg.min_payout_amount,
                        tolerance_percent: mint_cfg.balance_tolerance_percent,
                        payout_interval: Duration::from_secs(mint_cfg.payout_interval_seconds),
                        targets: profit_share
                            .into_iter()
                            .map(|ps| PayoutTarget {
                                identity: ps.identity,
                                factor: ps.factor,
                                lightning_address: String::new(),
                            })
                            .collect(),
                    };
                    let payout = v1::server::payout::spawn_payout_task(wallet.clone(), payout_cfg);
                    tokio::select! {
                        () = async { server.run(wallet).await } => {}
                        _ = payout => {
                            tracing::warn!("payout task finished");
                        }
                    }
                }
                #[cfg(feature = "spilman")]
                WalletType::Spilman => {
                    let wallet = Arc::new(
                        cdk_wallet::CdkWallet::new(&mint_url, [4u8; 64])
                            .await
                            .expect("failed to create CDK wallet"),
                    );
                    server.run(wallet).await;
                }
            }
        }
        Commands::V1Client {
            gateway,
            mac,
            mint_url,
            unit,
            preferred_allotment,
            poll_interval,
            renewal_threshold,
            max_price_per_ms,
            max_price_per_byte,
        } => {
            let wallet = Arc::new(
                cdk_wallet::CdkWallet::new(&mint_url, [3u8; 64])
                    .await
                    .expect("failed to create CDK wallet"),
            );

            let config = v1::V1ClientConfig {
                gateway_ip: gateway,
                mac_address: mac,
                our_mint_urls: vec![mint_url],
                unit,
                max_price_per_ms,
                max_price_per_byte,
                preferred_allotment,
                poll_interval_secs: poll_interval,
                renewal_threshold,
            };

            let mut client = v1::V1Client::<cdk_wallet::CdkWallet>::new(config);
            if let Err(e) = client.run(wallet).await {
                tracing::error!("v1 client failed: {e}");
                std::process::exit(1);
            }
        }
    }
}
