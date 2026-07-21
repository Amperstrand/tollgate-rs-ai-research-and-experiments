//! HTTP + WebSocket transport server.
//!
//! Endpoints (port 4747 by default — see `docs/design/core/tollgate-protocol.md`):
//!   POST /tollgate/v1/exchange   — HTTP polling (2-byte LE length-prefixed CBOR frames)
//!   GET  /tollgate/v1/ws        — WebSocket upgrade (one CBOR message per binary frame)

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use tollgate_protocol::{Announce, MessageType, decode_frames, encode_frame, peek_type};

use crate::driver::Driver;

pub async fn serve(
    listen: &str,
    driver: Driver,
    cfg: &crate::config::Config,
    #[cfg(feature = "v1-compat")] wallet: Option<
        std::sync::Arc<crate::v1_compat::wallet::CdkWallet>,
    >,
) -> anyhow::Result<()> {
    let app = router(
        driver,
        cfg,
        #[cfg(feature = "v1-compat")]
        wallet,
    );
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(feature = "v1-compat")]
pub fn build_v1_config(
    v1: &crate::config::V1CompatConfig,
    wallet: Option<std::sync::Arc<crate::v1_compat::wallet::CdkWallet>>,
) -> std::sync::Arc<crate::v1_compat::merchant::V1ServerConfig> {
    use crate::v1_compat::merchant::{AcceptedMint, V1ServerConfig};
    use std::sync::Arc;
    let metric = if v1.metric.is_empty() {
        "milliseconds".to_string()
    } else {
        v1.metric.clone()
    };
    let step_size = if v1.step_size == 0 {
        60_000
    } else {
        v1.step_size
    };
    let accepted_mints: Vec<AcceptedMint> = if v1.accepted_mints.is_empty() {
        vec![AcceptedMint {
            url: "http://localhost:3338".to_string(),
            price_per_step: 1,
            unit: "sat".to_string(),
            min_steps: 1,
        }]
    } else {
        v1.accepted_mints
            .iter()
            .map(|m| AcceptedMint {
                url: if m.url.is_empty() {
                    "http://localhost:3338".to_string()
                } else {
                    m.url.clone()
                },
                price_per_step: if m.price_per_step == 0 {
                    1
                } else {
                    m.price_per_step
                },
                unit: if m.unit.is_empty() {
                    "sat".to_string()
                } else {
                    m.unit.clone()
                },
                min_steps: if m.min_steps == 0 { 1 } else { m.min_steps },
            })
            .collect()
    };
    let nostr_keys = match &v1.nostr_secret_key {
        Some(hex) => nostr::key::Keys::parse(hex).unwrap_or_else(|_| nostr::key::Keys::generate()),
        None => nostr::key::Keys::generate(),
    };
    let mint_health = if !accepted_mints.is_empty() {
        let tracker = Arc::new(crate::v1_compat::mint_health::MintHealthTracker::new(
            &accepted_mints.iter().map(|m| m.url.clone()).collect::<Vec<_>>(),
        ));
        let tracker_clone = tracker.clone();
        tokio::spawn(async move {
            tracker_clone.run_initial_probe().await;
            tracing::info!("mint health initial probe complete");
        });
        tracker.start_proactive_checks();
        Some(tracker)
    } else {
        None
    };
    Arc::new(V1ServerConfig {
        metric,
        step_size,
        accepted_mints,
        nostr_keys,
        trust_proxy_headers: v1.trust_proxy_headers,
        wallet,
        mint_health,
    })
}

#[cfg_attr(not(feature = "v1-compat"), allow(unused_variables))]
pub fn router(
    driver: Driver,
    cfg: &crate::config::Config,
    #[cfg(feature = "v1-compat")] wallet: Option<
        std::sync::Arc<crate::v1_compat::wallet::CdkWallet>,
    >,
) -> Router {
    #[cfg(feature = "v1-compat")]
    let v1_driver = driver.clone();

    let base = Router::new()
        .route("/tollgate/v1/exchange", axum::routing::post(http_exchange))
        .route("/tollgate/v1/ws", get(ws_upgrade))
        .with_state(driver);

    #[cfg(feature = "v1-compat")]
    {
        let v1_config = build_v1_config(&cfg.v1_compat, wallet);
        base.merge(crate::v1_compat::build_v1_router(v1_driver, v1_config))
    }
    #[cfg(not(feature = "v1-compat"))]
    {
        base
    }
}

/// HTTP polling transport. The request body is zero or more length-prefixed
/// CBOR frames. We establish the peer from its Announce (first message of a
/// session), route the rest through the driver, and return any queued response
/// frames in the same framing.
async fn http_exchange(
    State(driver): State<Driver>,
    extensions: axum::http::Extensions,
    body: Bytes,
) -> Response {
    let frames = match decode_frames(&body) {
        Ok(f) => f,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("bad framing: {e:?}")).into_response();
        }
    };

    // ConnectInfo is injected into request extensions by
    // `into_make_service_with_connect_info`; absent in tests using `oneshot`.
    let peer_ip = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());

    // The Announce establishes the peer identity for this exchange. Without
    // transport-layer auth (the IP default), the pubkey comes from Announce.
    let mut peer_hex: Option<String> = None;

    for frame in frames {
        match peek_type(frame) {
            Some(MessageType::Announce) => match Announce::decode(frame) {
                Ok(announce) => {
                    let hex = hex::encode(announce.public_key().as_bytes());
                    // The HTTP transport re-sends Announce on every poll, so only the
                    // first (a genuinely new peer) is logged at INFO; the keep-alive
                    // repeats drop to DEBUG to keep the log readable.
                    if driver.peer_connected(&hex, peer_ip).await {
                        tracing::info!(
                            peer = %hex,
                            version = announce.version,
                            unit = %announce.unit,
                            ip = ?peer_ip,
                            "peer announced"
                        );
                    } else {
                        tracing::debug!(peer = %hex, ip = ?peer_ip, "peer re-announced");
                    }
                    peer_hex = Some(hex);
                }
                Err(e) => tracing::warn!(err = %e, "malformed Announce"),
            },
            Some(_) => match &peer_hex {
                Some(hex) => driver.message_received(hex, frame.to_vec()).await,
                None => tracing::warn!("message received before Announce; ignoring"),
            },
            None => tracing::warn!("unknown or malformed message; ignoring"),
        }
    }

    // Return any messages the driver queued for this peer during the exchange,
    // each as its own length-prefixed frame.
    let mut response = Vec::new();
    if let Some(hex) = &peer_hex {
        for message in driver.drain_outbox(hex).await {
            if encode_frame(&message, &mut response).is_err() {
                tracing::error!("queued message exceeds max frame length; dropping");
            }
        }
    }

    (StatusCode::OK, response).into_response()
}

/// WebSocket upgrade.
async fn ws_upgrade(State(_driver): State<Driver>) -> Response {
    // TODO: upgrade to a WebSocket, stream one CBOR message per binary frame
    // through the driver per connection.
    (
        StatusCode::NOT_IMPLEMENTED,
        "websocket transport not yet implemented",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::adapter::IpAdapter;
    use crate::config::{Config, Identity};
    use crate::wallet::BootstrapWallet;

    fn test_driver() -> Driver {
        let identity = Arc::new(Identity::load_or_generate(&Config::default()).unwrap());
        Driver::new(
            BootstrapWallet::new(vec![]),
            IpAdapter::new(),
            identity,
            tollgate_core::Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn announce_establishes_peer_and_returns_ok() {
        let app = router(
            test_driver(),
            &crate::config::Config::default(),
            #[cfg(feature = "v1-compat")]
            None,
        );

        let pk = tollgate_protocol::PublicKey::from_bytes([2u8; 33]);
        let announce = Announce::new(1, pk, "bytes", 0).encode();
        let body = tollgate_protocol::frame(&announce).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tollgate/v1/exchange")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_framing_is_rejected() {
        let app = router(
            test_driver(),
            &crate::config::Config::default(),
            #[cfg(feature = "v1-compat")]
            None,
        );

        // A length prefix claiming 9 bytes but with no body.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tollgate/v1/exchange")
                    .body(Body::from(vec![0x09, 0x00]))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
