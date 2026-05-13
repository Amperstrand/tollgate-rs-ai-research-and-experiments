//! V1 client: pays upstream TollGate routers using Cashu tokens.
//!
//! Implements the Chandler (client-side) flow from the Go v1 implementation:
//! 1. Fetch advertisement from gateway:2121
//! 2. Select cheapest compatible pricing
//! 3. Create Cashu token via CDK wallet
//! 4. POST token to gateway:2121
//! 5. Track usage via GET /usage polling
//! 6. Auto-renew before exhaustion

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

pub mod http;
pub mod nostr_events;
pub mod pricing;
pub mod server;

use std::sync::Arc;

use tollgate_core::wallet::Wallet;
use self::http::TollGateHttpClient;
use self::nostr_events::{PricingOption, SessionEvent, TollGateAdvertisement};


#[derive(Debug, thiserror::Error)]
pub enum V1ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] http::V1HttpError),
    #[error("pricing error: {0}")]
    Pricing(#[from] pricing::PricingError),
    #[error("wallet error: {0}")]
    Wallet(#[from] tollgate_core::error::WalletError),
    #[error("no active session")]
    NoSession,
}

/// Configuration for a v1 client connection.
#[allow(clippy::missing_errors_doc)]
pub struct V1ClientConfig {
    /// Gateway IP address of the upstream TollGate.
    pub gateway_ip: String,
    /// MAC address of our interface (used as device-identifier).
    pub mac_address: String,
    /// Mint URLs our wallet has funds in.
    pub our_mint_urls: Vec<String>,
    /// Currency unit we want to pay in (e.g., "sat").
    pub unit: String,
    /// Maximum price per millisecond we'll accept (0 = no limit).
    pub max_price_per_ms: f64,
    /// Maximum price per byte we'll accept (0 = no limit).
    pub max_price_per_byte: f64,
    /// Preferred allotment in milliseconds (for time-based) or bytes (for data-based).
    pub preferred_allotment: u64,
    /// Usage polling interval in seconds.
    pub poll_interval_secs: u64,
    /// Renewal threshold fraction (0.0–1.0). Renew when usage reaches this fraction of allotment.
    pub renewal_threshold: f64,
}

/// State for an active v1 client session with an upstream TollGate.
pub struct V1Session {
    pub advertisement: TollGateAdvertisement,
    pub selected_pricing: PricingOption,
    pub session_event: SessionEvent,
    pub total_allotment: u64,
    pub metric: String,
    pub step_size: u64,
}

/// V1 TollGate client. Manages a single upstream connection lifecycle.
///
/// Generic over `W: Wallet` to support both `CdkWallet` and `MockWallet`
/// (the `Wallet` trait uses `impl Future` returns and is not dyn-compatible).
pub struct V1Client<W: Wallet> {
    config: V1ClientConfig,
    http: TollGateHttpClient,
    session: Option<V1Session>,
    _wallet: std::marker::PhantomData<W>,
}

impl<W: Wallet> V1Client<W> {
    /// Create a new v1 client targeting the given gateway.
    pub fn new(config: V1ClientConfig) -> Self {
        let http = TollGateHttpClient::new(&config.gateway_ip);
        Self {
            config,
            http,
            session: None,
            _wallet: std::marker::PhantomData,
        }
    }

    /// Create a v1 client with an explicit HTTP base URL (for integration tests).
    pub fn new_with_base_url(config: V1ClientConfig, base_url: &str) -> Self {
        let http = TollGateHttpClient::new_with_base_url(base_url);
        Self {
            config,
            http,
            session: None,
            _wallet: std::marker::PhantomData,
        }
    }

    /// Access the current session, if any.
    pub fn session(&self) -> Option<&V1Session> {
        self.session.as_ref()
    }

    /// Connect to the upstream TollGate: fetch ad, select pricing, pay.
    pub async fn connect(&mut self, wallet: &Arc<W>) -> Result<(), V1ClientError> {
        // Step 1: Check for existing session via /usage
        let (usage, allotment) = self.http.fetch_usage().await?;
        if allotment > 0 {
            tracing::info!(
                usage,
                allotment,
                "Existing session found, re-attaching without new payment"
            );
            // We have a session but no ad — fetch it for metadata
            let ad = self.http.fetch_advertisement().await?;
            let metric = ad.metric().unwrap_or_else(|| "milliseconds".into());
            let step_size = ad.step_size().unwrap_or(60_000);
            let options = ad.pricing_options();
            let pricing = pricing::select_cheapest_compatible(
                &options,
                &self.config.our_mint_urls,
                &self.config.unit,
            )?
            .clone();

            self.session = Some(V1Session {
                advertisement: ad,
                selected_pricing: pricing,
                session_event: SessionEvent::from_json("{}").unwrap_or_else(|_| {
                    panic!("should not happen: empty JSON for recovered session")
                }),
                total_allotment: allotment.max(0) as u64,
                metric,
                step_size,
            });
            tracing::info!("Re-attached to existing session");
            return Ok(());
        }

        // Step 2: Fetch advertisement
        let ad = self.http.fetch_advertisement().await?;
        let metric = ad.metric().unwrap_or_else(|| "milliseconds".into());
        let step_size = ad.step_size().unwrap_or(60_000);

        // Step 3: Select cheapest compatible pricing
        let options = ad.pricing_options();
        let pricing = pricing::select_cheapest_compatible(
            &options,
            &self.config.our_mint_urls,
            &self.config.unit,
        )?;

        // Step 4: Validate budget
        pricing::validate_budget(
            pricing,
            step_size,
            &metric,
            self.config.max_price_per_ms,
            self.config.max_price_per_byte,
        )?;

        // Step 5: Calculate steps
        let preferred_steps = self
            .config
            .preferred_allotment
            .checked_div(step_size)
            .unwrap_or(1);
        let balance = wallet.balance().await?;
        let balance_sats = balance.0;
        let max_affordable = balance_sats
            .checked_div(pricing.price_per_step)
            .unwrap_or(0);

        let steps = preferred_steps
            .max(pricing.min_steps)
            .min(max_affordable)
            .max(1);

        let payment_amount = steps * pricing.price_per_step;
        tracing::info!(
            steps,
            payment_amount,
            balance_sats,
            price_per_step = pricing.price_per_step,
            "Creating payment"
        );

        // Step 6: Create token and send
        let token_bytes = wallet
            .create_token(
                tollgate_core::types::Amount(payment_amount),
                &pricing.mint_url,
            )
            .await?;
        let token_str = String::from_utf8_lossy(&token_bytes).to_string();

        let session_event = self.http.send_payment(&token_str).await?;
        let allotment = session_event.allotment().unwrap_or(0);

        tracing::info!(
            allotment,
            metric,
            steps,
            amount_paid = payment_amount,
            "Session established"
        );

        self.session = Some(V1Session {
            advertisement: ad,
            selected_pricing: pricing.clone(),
            session_event,
            total_allotment: allotment,
            metric,
            step_size,
        });

        Ok(())
    }

    /// Poll usage and return (usage, allotment, needs_renewal).
    pub async fn poll_usage(&self) -> (u64, u64, bool) {
        match self.http.fetch_usage().await {
            Ok((usage, allotment)) => {
                let needs_renewal = if allotment > 0 {
                    let ratio = usage as f64 / allotment as f64;
                    ratio >= self.config.renewal_threshold
                } else {
                    false
                };
                (usage.max(0) as u64, allotment.max(0) as u64, needs_renewal)
            }
            Err(e) => {
                tracing::warn!("Usage poll failed: {e}");
                (0, 0, false)
            }
        }
    }

    /// Renew the session by making another payment.
    pub async fn renew(&mut self, wallet: &Arc<W>) -> Result<(), V1ClientError> {
        let session = self.session.as_ref().ok_or(V1ClientError::NoSession)?;

        let step_size = session.step_size;
        let pricing = &session.selected_pricing;

        let preferred_steps = self
            .config
            .preferred_allotment
            .checked_div(step_size)
            .unwrap_or(1);
        let balance = wallet.balance().await?;
        let max_affordable = balance.0.checked_div(pricing.price_per_step).unwrap_or(0);

        let steps = preferred_steps
            .max(pricing.min_steps)
            .min(max_affordable)
            .max(1);

        let payment_amount = steps * pricing.price_per_step;
        tracing::info!(steps, payment_amount, "Renewing session");

        let token_bytes = wallet
            .create_token(
                tollgate_core::types::Amount(payment_amount),
                &pricing.mint_url,
            )
            .await?;
        let token_str = String::from_utf8_lossy(&token_bytes).to_string();

        let session_event = self.http.send_payment(&token_str).await?;
        let allotment = session_event.allotment().unwrap_or(0);

        if let Some(s) = &mut self.session {
            s.total_allotment = allotment;
            s.session_event = session_event;
        }

        tracing::info!(allotment, "Session renewed");
        Ok(())
    }

    /// Run the client loop: connect, poll usage, auto-renew.
    pub async fn run(&mut self, wallet: Arc<W>) -> Result<(), V1ClientError> {
        self.connect(&wallet).await?;

        let poll_interval = tokio::time::Duration::from_secs(self.config.poll_interval_secs);

        loop {
            tokio::time::sleep(poll_interval).await;

            let (usage, allotment, needs_renewal) = self.poll_usage().await;

            if allotment == 0 {
                tracing::warn!("Session lost (allotment=0), reconnecting...");
                self.connect(&wallet).await?;
                continue;
            }

            tracing::debug!(usage, allotment, "Usage poll");

            if needs_renewal {
                tracing::info!(
                    usage,
                    allotment,
                    threshold = self.config.renewal_threshold,
                    "Approaching allotment, renewing"
                );
                if let Err(e) = self.renew(&wallet).await {
                    tracing::error!("Renewal failed: {e}");
                }
            }
        }
    }
}
