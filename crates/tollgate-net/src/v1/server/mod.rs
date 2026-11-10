#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod config;
pub mod handlers;
pub mod janitor;
pub mod lightning_quotes;
pub mod logging;
pub mod mac_resolver;
pub mod merchant;
pub mod mint_quote_wallet;
pub mod payout;
pub mod session_store;
pub mod upstream_detector;
pub mod valve;

use std::sync::Arc;
use std::time::Duration;

use nostr::prelude::*;
use tollgate_core::wallet::Wallet;

pub use config::{
    load_or_generate_keys, ConfigError, KeyError, MintConfig as FileMintConfig, ProfitShareConfig,
    ServerConfig,
};
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
pub use mint_quote_wallet::{
    MintQuoteError, MintQuoteInfo, MintQuoteWallet, MintResult, MockMintQuoteWallet, QuoteState,
};
pub use session_store::{
    InMemorySessionStore, SessionStore, SessionStoreError, SqliteSessionStore,
};
pub use upstream_detector::{
    parse_advertisement, probe_gateway, probe_url, DiscoveredUpstream, UpstreamDetectError,
    UpstreamDetectorConfig, UpstreamMint,
};
pub use valve::{ClientStats, StubValve, Valve, ValveError};

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

pub struct ServerState<W: Wallet> {
    pub wallet: Arc<W>,
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
}

impl V1Server {
    pub fn new(config: V1ServerConfig) -> Self {
        Self { config }
    }

    /// Assemble the server state, wiring both the payment wallet and the
    /// mint-quote view of that same wallet.
    ///
    /// The wallet passed to `run` implements both traits (CDK wallet in
    /// production, mock in tests); wiring `mint_quote_wallet` from it keeps
    /// POST /ln-invoice functional — leaving it `None` dead-ends the
    /// lightning invoice flow.
    fn build_state<W: Wallet + MintQuoteWallet + 'static>(
        config: V1ServerConfig,
        wallet: Arc<W>,
        valve: Arc<dyn Valve + Send + Sync>,
    ) -> Arc<ServerState<W>> {
        let advertisement =
            merchant::build_advertisement(&config).expect("failed to build advertisement");

        Arc::new(ServerState {
            wallet: wallet.clone(),
            mint_quote_wallet: Some(wallet),
            config,
            sessions: Arc::new(InMemorySessionStore::new()),
            mac_resolver: Arc::new(StubMacResolver::default()),
            valve,
            lightning_quotes: Arc::new(InMemoryLightningQuoteStore::new()),
            advertisement,
        })
    }

    pub async fn run<W: Wallet + MintQuoteWallet + 'static>(
        self,
        wallet: Arc<W>,
        valve: Arc<dyn Valve + Send + Sync>,
    ) {
        let port = self.config.port;
        let state = Self::build_state(self.config, wallet, valve);

        let janitor = spawn_janitor(
            state.sessions.clone(),
            state.valve.clone(),
            Duration::from_secs(5),
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
            _ = quote_janitor => {
                tracing::warn!("quote janitor task finished unexpectedly");
            }
        }
    }
}

#[cfg(test)]
mod run_wiring_tests {
    use super::*;
    use crate::mock::MockWallet;

    fn test_config() -> V1ServerConfig {
        V1ServerConfig {
            metric: "milliseconds".to_owned(),
            step_size: 60_000,
            accepted_mints: vec![AcceptedMint {
                url: "https://mint.example".to_owned(),
                price_per_step: 1,
                unit: "sat".to_owned(),
                min_steps: 1,
            }],
            nostr_keys: Keys::generate(),
            port: 0,
        }
    }

    #[tokio::test]
    async fn run_wires_mint_quote_wallet_from_the_payment_wallet() {
        let state = V1Server::build_state(
            test_config(),
            Arc::new(MockWallet::new(0)),
            Arc::new(StubValve),
        );

        let quote_wallet = state.mint_quote_wallet.as_ref().expect(
            "production run() must wire mint_quote_wallet — None dead-ends POST /ln-invoice",
        );

        let quote = quote_wallet
            .request_mint_quote(21, "https://mint.example")
            .await
            .expect("wired quote wallet must be functional");
        assert_eq!(quote.amount, 21);
        assert_ne!(quote.invoice, "");
    }
}
