//! V1 HTTP/JSON compatibility server (port 2121).
//!
//! Implements the Go v1 TollGate wire protocol so the Rust binary can serve as
//! a drop-in replacement for the Go backend.  This runs alongside (or instead
//! of) the v2 CBOR server.
//!
//! See `docs/design/core/tollgate-protocol.md` and the Go v1 reference at
//! <https://github.com/OpenTollGate/tollgate-module-basic-go>.

mod handlers;
mod nostr;
mod session;

pub use handlers::{V1Config, V1State};

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;

use crate::adapter::IpAdapter;
use crate::config::Identity;
use crate::wallet::BootstrapWallet;

use session::V1SessionStore;

/// The v1 HTTP/JSON server.  Construct with [`V1Server::new`], then call
/// [`V1Server::serve`] to bind and serve.
pub struct V1Server {
    state: Arc<V1State>,
}

impl V1Server {
    /// Create a new v1 server, precomputing the advertisement event.
    pub fn new(
        identity: &Identity,
        wallet: BootstrapWallet,
        adapter: IpAdapter,
        config: V1Config,
    ) -> anyhow::Result<Self> {
        let secret_key = *identity.secret_key();
        let xonly_pubkey_hex = identity.xonly_pubkey_hex();

        let advertisement =
            handlers::build_advertisement_json(&xonly_pubkey_hex, &secret_key, &config)
                .context("failed to build advertisement")?;

        tracing::info!(
            pubkey = %xonly_pubkey_hex,
            listen_hint = "port 2121",
            "v1 server initialized",
        );

        let state = Arc::new(V1State {
            advertisement,
            secret_key_bytes: secret_key.secret_bytes(),
            xonly_pubkey_hex,
            wallet,
            adapter,
            sessions: V1SessionStore::new(),
            config,
        });

        Ok(Self { state })
    }

    /// Bind to `listen` and serve the v1 HTTP/JSON API.  Blocks until the server
    /// shuts down.
    pub async fn serve(self, listen: &str) -> anyhow::Result<()> {
        let app = handlers::build_router(self.state);
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("binding {listen}"))?;
        let addr = listener.local_addr().context("local_addr")?;
        tracing::info!(%addr, "v1 HTTP/JSON server listening");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .context("axum serve")?;
        Ok(())
    }
}
