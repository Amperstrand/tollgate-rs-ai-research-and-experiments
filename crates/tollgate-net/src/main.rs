use clap::{Parser, Subcommand};

mod client;
mod mock;
mod server;

#[derive(Parser)]
#[command(name = "tollgate-net", about = "TollGate v2 network node")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as a provider (sells network access)
    Provider {
        /// Port to listen on
        #[arg(long, default_value = "3001")]
        port: u16,
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
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("tollgate_net=info,tollgate_core=info")
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Provider { port } => server::run(port).await,
        Commands::Client {
            peer,
            intervals,
            interval_secs,
        } => client::run(&peer, intervals, interval_secs).await,
    }
}
