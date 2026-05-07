use std::sync::Arc;

use clap::{Parser, Subcommand};
use tollgate_net::{cdk_wallet, client, mock, server};

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
        #[arg(long, default_value = "https://testnut.cashu.space")]
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
        #[arg(long, default_value = "https://testnut.cashu.space")]
        mint_url: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=info,tollgate_core=info")
        .init();

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
        },
        Commands::Client {
            peer,
            intervals,
            interval_secs,
            wallet: wt,
            mint_url,
        } => match wt {
            WalletType::Mock => {
                client::run_mock(&peer, intervals, interval_secs, 200).await;
            }
            WalletType::Cdk => {
                let wallet = Arc::new(
                    cdk_wallet::CdkWallet::new(&mint_url, [2u8; 64])
                        .await
                        .expect("failed to create CDK wallet"),
                );
                client::run_cdk(&peer, intervals, interval_secs, wallet, &mint_url).await;
            }
        },
    }
}
