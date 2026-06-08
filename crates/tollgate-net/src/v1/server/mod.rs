#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod config;
pub mod data_monitor;
pub mod degraded_wallet;
pub mod handlers;
pub mod install_config;
pub mod janitor;
pub mod lightning_quotes;
pub mod logging;
pub mod mac_resolver;
pub mod merchant;
pub mod merchant_provider;
pub mod mint_health_tracker;
pub mod mint_quote_wallet;
pub mod payout;
pub mod session_store;
pub mod ubus_client;
pub mod uci_ops;
pub mod upstream_detector;
pub mod upstream_manager;
pub mod valve;
pub mod wifi_connector;
pub mod wifi_scanner;

#[cfg(all(target_os = "linux", feature = "netlink"))]
mod network_monitor;

#[cfg(not(all(target_os = "linux", feature = "netlink")))]
#[path = "network_monitor_stub.rs"]
mod network_monitor;

use std::sync::Arc;
use std::time::Duration;

use nostr::prelude::*;

pub use config::{
    load_or_generate_keys, ConfigError, Identities, KeyError, MintConfig as FileMintConfig,
    OwnedIdentity, ProfitShareConfig, PublicIdentity, ServerConfig, CONFIG_SCHEMA_VERSION,
};
pub use data_monitor::spawn_data_monitor;
pub use degraded_wallet::DegradedWallet;
pub use install_config::InstallConfig;
pub use janitor::spawn_janitor;
pub use lightning_quotes::{
    spawn_quote_janitor, InMemoryLightningQuoteStore, LightningQuoteRecord, LightningQuoteStore,
    QuoteStoreError,
};
pub use logging::init_logging;
pub use mac_resolver::{
    extract_client_ip, DhcpLeasesResolver, MacResolveError, MacResolver, StubMacResolver,
};
pub use merchant::{
    build_advertisement, build_notice_event, build_session_event, calculate_allotment,
    AllotmentError,
};
pub use merchant_provider::MerchantProvider;
pub use mint_health_tracker::MintHealthTracker;
pub use mint_quote_wallet::{
    MintQuoteError, MintQuoteInfo, MintQuoteWallet, MintResult, MockMintQuoteWallet, QuoteState,
};
pub use session_store::{
    InMemorySessionStore, SessionStore, SessionStoreError, SqliteSessionStore,
};
pub use ubus_client::{RadioInfo, UbusClient, UbusError};
pub use uci_ops::{
    execute_shell as execute_uci_shell, render_shell as render_uci_shell, sh_quote,
    validate_identifier, OpValue, ServiceAction, UciOp, UciOpBuilder, UciOpError,
};
pub use upstream_detector::{
    parse_advertisement, probe_gateway, probe_url, DiscoveredUpstream, UpstreamDetectError,
    UpstreamDetectorConfig, UpstreamMint,
};
pub use upstream_manager::{
    Blacklist, CircuitBreaker, ManagerState, ScanCycleResult, ScanReason, SwitchCandidate,
    UpstreamError, UpstreamManager, UpstreamManagerConfig,
};
pub use valve::{ClientStats, StubValve, Valve, ValveError};
pub use wifi_connector::{WifiConnectError, WifiConnector};
pub use wifi_scanner::{
    CommandExecutor, CommandOutput, EncryptionType, ScanResult, SystemCommandExecutor,
    WifiScanError, WifiScanner,
};

pub use network_monitor::{
    InterfaceInfo, NetworkEvent, NetworkMonitor, NetworkMonitorConfig, NetworkMonitorError,
};

#[cfg(feature = "nds")]
pub use valve::NdsValve;

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

pub struct ServerState {
    pub merchant: Arc<MerchantProvider>,
    pub config: V1ServerConfig,
    pub sessions: Arc<dyn SessionStore>,
    pub mac_resolver: Arc<dyn MacResolver + Send + Sync>,
    pub valve: Arc<dyn Valve + Send + Sync>,
    pub mint_quote_wallet: Option<Arc<dyn MintQuoteWallet>>,
    pub lightning_quotes: Arc<dyn LightningQuoteStore>,
    pub advertisement: String,
}

pub struct V1Server {
    config: V1ServerConfig,
    sessions: Option<Arc<dyn SessionStore>>,
    mac_resolver: Option<Arc<dyn MacResolver + Send + Sync>>,
    mint_quote_wallet: Option<Arc<dyn MintQuoteWallet>>,
}

impl V1Server {
    pub fn new(config: V1ServerConfig) -> Self {
        Self {
            config,
            sessions: None,
            mac_resolver: None,
            mint_quote_wallet: None,
        }
    }

    /// Override the session store (defaults to in-memory).
    #[must_use]
    pub fn with_session_store(mut self, sessions: Arc<dyn SessionStore>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Override the MAC resolver (defaults to a fixed stub MAC).
    #[must_use]
    pub fn with_mac_resolver(mut self, resolver: Arc<dyn MacResolver + Send + Sync>) -> Self {
        self.mac_resolver = Some(resolver);
        self
    }

    /// Enable Lightning invoice endpoints by supplying a mint-quote wallet.
    ///
    /// Without this, `/ln-invoice` returns "lightning payments not available".
    #[must_use]
    pub fn with_mint_quote_wallet(mut self, wallet: Arc<dyn MintQuoteWallet>) -> Self {
        self.mint_quote_wallet = Some(wallet);
        self
    }

    pub async fn run(self, merchant: Arc<MerchantProvider>, valve: Arc<dyn Valve + Send + Sync>) {
        let port = self.config.port;
        let advertisement =
            merchant::build_advertisement(&self.config).expect("failed to build advertisement");

        let sessions = self
            .sessions
            .unwrap_or_else(|| Arc::new(InMemorySessionStore::new()));
        let mac_resolver = self
            .mac_resolver
            .unwrap_or_else(|| Arc::new(StubMacResolver::default()));

        let state = Arc::new(ServerState {
            merchant,
            config: self.config,
            sessions,
            mac_resolver,
            valve,
            mint_quote_wallet: self.mint_quote_wallet,
            lightning_quotes: Arc::new(InMemoryLightningQuoteStore::new()),
            advertisement,
        });

        let janitor = spawn_janitor(
            state.sessions.clone(),
            state.valve.clone(),
            Duration::from_secs(5),
        );

        let data_monitor = data_monitor::spawn_data_monitor(
            state.sessions.clone(),
            state.valve.clone(),
            Duration::from_secs(2),
        );

        let quote_janitor = lightning_quotes::spawn_quote_janitor(
            state.lightning_quotes.clone(),
            Duration::from_secs(60),
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
            _ = data_monitor => {
                tracing::warn!("data monitor task finished unexpectedly");
            }
            _ = quote_janitor => {
                tracing::warn!("quote janitor task finished unexpectedly");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received shutdown signal, stopping server...");
            }
        }
    }
}
