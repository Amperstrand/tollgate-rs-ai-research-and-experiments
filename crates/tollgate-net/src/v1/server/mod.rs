#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod handlers;
pub mod mac_resolver;
pub mod merchant;
pub mod valve;

use std::collections::HashMap;
use std::sync::Arc;

use nostr::prelude::*;
use tokio::sync::Mutex;
use tollgate_core::wallet::Wallet;

pub use mac_resolver::{DhcpLeasesResolver, MacResolveError, MacResolver, StubMacResolver};
pub use merchant::{AllotmentError, build_advertisement, build_notice_event, build_session_event, calculate_allotment};
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

#[derive(Clone)]
pub struct CustomerSession {
    pub mac_address: String,
    pub start_time: i64,
    pub metric: String,
    pub allotment: u64,
}

pub struct ServerState<W: Wallet> {
    pub wallet: Arc<W>,
    pub config: V1ServerConfig,
    pub sessions: Mutex<HashMap<String, CustomerSession>>,
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
            sessions: Mutex::new(HashMap::new()),
            mac_resolver: Arc::new(StubMacResolver::default()),
            valve: Arc::new(StubValve),
            advertisement,
        });

        let app = handlers::build_router(state);

        let addr = format!("0.0.0.0:{port}");
        tracing::info!("v1 server listening on port {port}");

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .expect("failed to bind listener");

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server exited with error");
    }
}
