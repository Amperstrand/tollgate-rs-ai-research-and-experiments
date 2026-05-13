#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod config;
pub mod handlers;
pub mod janitor;
pub mod logging;
pub mod mac_resolver;
pub mod merchant;
pub mod payout;
pub mod session_store;
pub mod upstream_detector;
pub mod valve;

use std::sync::Arc;
use std::time::Duration;

use nostr::prelude::*;
use tollgate_core::wallet::Wallet;

pub use config::{ConfigError, MintConfig as FileMintConfig, ProfitShareConfig, ServerConfig};
pub use janitor::spawn_janitor;
pub use logging::init_logging;
pub use mac_resolver::{DhcpLeasesResolver, MacResolveError, MacResolver, StubMacResolver};
pub use merchant::{
    build_advertisement, build_notice_event, build_session_event, calculate_allotment,
    AllotmentError,
};
pub use session_store::{
    InMemorySessionStore, SessionStore, SessionStoreError, SqliteSessionStore,
};
pub use upstream_detector::{
    parse_advertisement, probe_gateway, probe_url, DiscoveredUpstream, UpstreamDetectError,
    UpstreamDetectorConfig, UpstreamMint,
};
pub use valve::{StubValve, Valve, ValveError};

pub struct AcceptedMint {
    pub url: String,
    pub price_per_step: u64,
    pub unit: String,
    pub min_steps: u64,
}

pub struct V1ServerConfig {
    pub metric: String,
    pub step_size: u64,
    pub accepted_mints: Vec<AcceptedMint>,
    pub nostr_keys: Keys,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CustomerSession {
    pub mac_address: String,
    pub start_time: i64,
    pub metric: String,
    pub allotment: u64,
}

pub struct ServerState<W: Wallet> {
    pub wallet: Arc<W>,
    pub config: V1ServerConfig,
    pub sessions: Arc<dyn SessionStore>,
    pub mac_resolver: Arc<dyn MacResolver + Send + Sync>,
    pub valve: Arc<dyn Valve + Send + Sync>,
    pub advertisement: String,
}

pub struct V1Server {
    config: V1ServerConfig,
}

impl V1Server {
    pub fn new(config: V1ServerConfig) -> Self {
        Self { config }
    }

    pub async fn run<W: Wallet + 'static>(self, wallet: Arc<W>) {
        let port = self.config.port;
        let advertisement =
            merchant::build_advertisement(&self.config).expect("failed to build advertisement");

        let state = Arc::new(ServerState {
            wallet,
            config: self.config,
            sessions: Arc::new(InMemorySessionStore::new()),
            mac_resolver: Arc::new(StubMacResolver::default()),
            valve: Arc::new(StubValve),
            advertisement,
        });

        let janitor = spawn_janitor(
            state.sessions.clone(),
            state.valve.clone(),
            Duration::from_secs(5),
        );

        let app = handlers::build_router(state);

        let addr = format!("0.0.0.0:{port}");
        tracing::info!("v1 server listening on port {port}");

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .expect("failed to bind listener");

        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );

        tokio::select! {
            result = server => {
                if let Err(e) = result {
                    tracing::error!("HTTP server error: {e}");
                }
            }
            _ = janitor => {
                tracing::warn!("janitor task finished unexpectedly");
            }
        }
    }
}
