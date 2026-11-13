//! V1 client: pays upstream TollGate routers using Cashu tokens.
//!
//! Implements the Chandler (client-side) flow from the Go v1 implementation:
//! 1. Fetch advertisement from gateway:2121
//! 2. Select cheapest compatible pricing
//! 3. Create Cashu token via CDK wallet
//! 4. POST token to gateway:2121
//! 5. Track usage via GET /usage polling
//! 6. Auto-renew before exhaustion
//!
//! Ported from the experimental v1 archive into the v1-compat layer.
//! Uses concrete [`CdkWallet`](super::wallet::CdkWallet) instead of a
//! generic `W: Wallet` — the experimental `tollgate_core::wallet::Wallet`
//! trait does not exist upstream.

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use super::http_client::{TollGateHttpClient, V1HttpError};
use super::nostr::{SessionEvent, TollGateAdvertisement};
use super::pricing;
use super::pricing::PricingOption;
use super::recovery;
use super::wallet::CdkWallet;

/// Wallet operations the v1 client needs. Implemented by
/// [`CdkWallet`] in production and by offline mocks in tests, so the
/// client's money-path guards can be regression-pinned without a live
/// mint.
pub trait V1ClientWallet {
    fn balance(&self) -> impl Future<Output = anyhow::Result<u64>> + Send;

    fn create_token_with_overpayment(
        &self,
        amount: u64,
        mint_url: &str,
        max_overpayment_absolute: u64,
    ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send;

    fn receive_token(&self, token: &[u8]) -> impl Future<Output = anyhow::Result<u64>> + Send;
}

impl V1ClientWallet for CdkWallet {
    fn balance(&self) -> impl Future<Output = anyhow::Result<u64>> + Send {
        CdkWallet::balance(self)
    }

    fn create_token_with_overpayment(
        &self,
        amount: u64,
        mint_url: &str,
        max_overpayment_absolute: u64,
    ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
        CdkWallet::create_token_with_overpayment(self, amount, mint_url, max_overpayment_absolute)
    }

    fn receive_token(&self, token: &[u8]) -> impl Future<Output = anyhow::Result<u64>> + Send {
        CdkWallet::receive_token(self, token)
    }
}

/// Convert a [`nostr::PricingOption`] (from advertisement parsing) into a
/// [`pricing::PricingOption`] (used by pricing selection logic).
///
/// The two structs are field-identical but distinct types in the v1-compat
/// layer — `nostr::PricingOption` is embedded in parsed Nostr events, while
/// `pricing::PricingOption` is the standalone type used by pricing functions.
fn to_pricing_option(o: &super::nostr::PricingOption) -> PricingOption {
    PricingOption {
        asset_type: o.asset_type.clone(),
        price_per_step: o.price_per_step,
        unit: o.unit.clone(),
        mint_url: o.mint_url.clone(),
        min_steps: o.min_steps,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum V1ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] V1HttpError),
    #[error("pricing error: {0}")]
    Pricing(#[from] pricing::PricingError),
    #[error("wallet error: {0}")]
    Wallet(anyhow::Error),
    #[error("no active session")]
    NoSession,
    #[error("Lightning payment timeout: invoice not paid within {0}s")]
    LnPaymentTimeout(u64),
    #[error("unexpected error: {0}")]
    Unexpected(String),
}

#[derive(Clone)]
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
    /// Where unrecoverable outgoing tokens are appended for manual
    /// recovery when a gateway payment fails after token creation
    /// (see [`recovery::recover_failed_token`]). Production wiring
    /// points at `/etc/tollgate/tokens-to-recover.txt`.
    pub recovery_file: PathBuf,
}

/// State for an active v1 client session with an upstream TollGate.
pub struct V1Session {
    pub advertisement: TollGateAdvertisement,
    pub selected_pricing: PricingOption,
    /// Session event from the upstream TollGate (kind 1022).
    /// `None` when re-attaching to an existing session where no event was received.
    pub session_event: Option<SessionEvent>,
    pub total_allotment: u64,
    pub metric: String,
    pub step_size: u64,
}

/// Minimum time between payment attempts (prevents renewal storms).
const MIN_PAYMENT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// V1 TollGate client. Manages a single upstream connection lifecycle.
///
/// Uses [`CdkWallet`] for Cashu token creation. The client is a standalone
/// HTTP client that pays Go v1 routers via the TIP-03 protocol.
pub struct V1Client {
    config: V1ClientConfig,
    http: TollGateHttpClient,
    session: Option<V1Session>,
    last_payment_attempt: Option<std::time::Instant>,
}

impl V1Client {
    /// Create a new v1 client targeting the given gateway.
    pub fn new(config: V1ClientConfig) -> Self {
        let http = TollGateHttpClient::new(&config.gateway_ip);
        Self {
            config,
            http,
            session: None,
            last_payment_attempt: None,
        }
    }

    /// Create a v1 client with an explicit HTTP base URL (for integration tests).
    pub fn new_with_base_url(config: V1ClientConfig, base_url: &str) -> Self {
        let http = TollGateHttpClient::new_with_base_url(base_url);
        Self {
            config,
            http,
            session: None,
            last_payment_attempt: None,
        }
    }

    /// Access the current session, if any.
    pub fn session(&self) -> Option<&V1Session> {
        self.session.as_ref()
    }

    /// Connect to the upstream TollGate: fetch ad, select pricing, pay.
    pub async fn connect<W: V1ClientWallet + 'static>(
        &mut self,
        wallet: &Arc<W>,
    ) -> Result<(), V1ClientError> {
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
            let options: Vec<PricingOption> =
                ad.pricing_options().iter().map(to_pricing_option).collect();
            let pricing = pricing::select_cheapest_compatible(
                &options,
                &self.config.our_mint_urls,
                &self.config.unit,
            )?;

            // Reattached sessions renew at the advertised price: the same
            // budget cap as a fresh connection applies, or a restart with a
            // stricter cap would keep auto-paying above it.
            pricing::validate_budget(
                pricing,
                step_size,
                &metric,
                self.config.max_price_per_ms,
                self.config.max_price_per_byte,
            )?;

            self.session = Some(V1Session {
                advertisement: ad,
                selected_pricing: pricing.clone(),
                session_event: None,
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
        let options: Vec<PricingOption> =
            ad.pricing_options().iter().map(to_pricing_option).collect();
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
        let balance = wallet.balance().await.map_err(V1ClientError::Wallet)?;
        let balance_sats = balance;
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

        if max_affordable == 0 || balance_sats == 0 {
            tracing::info!("No Cashu balance, attempting Lightning payment");
            return self.connect_via_lightning(&ad, pricing, steps).await;
        }

        // A partial balance below the minimum would buy an undersized
        // token: the gateway redeems it, then rejects the session with
        // InsufficientSteps — the client loses the funds and no recovery
        // path exists for an already-redeemed token.
        if max_affordable < pricing.min_steps {
            return Err(V1ClientError::Pricing(
                pricing::PricingError::InsufficientFunds {
                    required: pricing.min_steps * pricing.price_per_step,
                    available: balance_sats,
                },
            ));
        }

        // Step 6: Create token and send
        let token_bytes = wallet
            .create_token_with_overpayment(payment_amount, &pricing.mint_url, 100)
            .await
            .map_err(V1ClientError::Wallet)?;
        let token_str = String::from_utf8_lossy(&token_bytes).to_string();

        let session_event = self
            .submit_payment_with_recovery(
                wallet.as_ref(),
                &token_bytes,
                &token_str,
                &pricing.mint_url,
            )
            .await?;
        let allotment = session_event.allotment().unwrap_or(0);

        tracing::info!(
            allotment,
            metric,
            steps,
            amount_paid = payment_amount,
            "Session established"
        );

        let ad_clone = ad.clone();
        self.session = Some(V1Session {
            advertisement: ad_clone,
            selected_pricing: pricing.clone(),
            session_event: Some(session_event),
            total_allotment: allotment,
            metric,
            step_size,
        });

        Ok(())
    }

    async fn connect_via_lightning(
        &mut self,
        ad: &TollGateAdvertisement,
        pricing: &PricingOption,
        steps: u64,
    ) -> Result<(), V1ClientError> {
        let amount = steps * pricing.price_per_step;
        let mint_url = pricing.mint_url.clone();

        tracing::info!(steps, amount, "Requesting Lightning invoice");

        let resp = self.http.request_ln_invoice(amount, &mint_url).await?;
        let quote = resp
            .quote
            .as_deref()
            .ok_or_else(|| V1ClientError::Unexpected("No quote ID in response".to_string()))?;

        println!("BOLT11 Invoice:");
        println!("{}", resp.invoice.as_ref().unwrap_or(&String::new()));
        println!(
            "\nPlease pay this invoice using your Lightning wallet and wait for the server to grant access."
        );
        println!("Quote ID: {quote}");
        println!("Expires in: {} seconds", resp.expiry.unwrap_or(0));

        let timeout = tokio::time::Duration::from_secs(300); // 5 minutes
        let poll_interval = tokio::time::Duration::from_secs(3);

        let start = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(poll_interval).await;

            if start.elapsed() > timeout {
                return Err(V1ClientError::LnPaymentTimeout(300));
            }

            let status = self.http.check_ln_invoice_status(quote).await?;

            if status.state == "ISSUED" {
                if let Some(allotment) = status.allotment {
                    let ad_clone = ad.clone();
                    let metric = ad_clone.metric().unwrap_or_else(|| "milliseconds".into());
                    let step_size = ad_clone.step_size().unwrap_or(60_000);

                    self.session = Some(V1Session {
                        advertisement: ad_clone,
                        selected_pricing: pricing.clone(),
                        session_event: None,
                        total_allotment: allotment,
                        metric: metric.clone(),
                        step_size,
                    });

                    tracing::info!(
                        allotment,
                        %metric,
                        "Lightning payment succeeded, session established"
                    );
                    return Ok(());
                }
            }
        }
    }

    /// Submit a payment token upstream. If submission fails after the token
    /// was already created (funds left the wallet), reclaim it via
    /// [`recovery::recover_failed_token`] before propagating the error, so a
    /// failed connection attempt never strands the client's funds.
    async fn submit_payment_with_recovery<W: V1ClientWallet + 'static>(
        &self,
        wallet: &W,
        token_bytes: &[u8],
        token_str: &str,
        mint_url: &str,
    ) -> Result<SessionEvent, V1ClientError> {
        match self.http.send_payment(token_str).await {
            Ok(event) => Ok(event),
            Err(e) => {
                let outcome = recovery::recover_failed_token(
                    wallet,
                    token_bytes,
                    mint_url,
                    &e.to_string(),
                    &self.config.recovery_file,
                )
                .await;
                tracing::warn!(
                    ?outcome,
                    gateway = %self.config.gateway_ip,
                    "payment submission failed after token creation; recovery attempted"
                );
                Err(e.into())
            }
        }
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
    pub async fn renew<W: V1ClientWallet + 'static>(
        &mut self,
        wallet: &Arc<W>,
    ) -> Result<(), V1ClientError> {
        if let Some(last) = self.last_payment_attempt {
            let elapsed = last.elapsed();
            if elapsed < MIN_PAYMENT_INTERVAL {
                tracing::debug!(
                    elapsed_ms = elapsed.as_millis(),
                    "Skipping renewal, too soon since last payment attempt"
                );
                return Ok(());
            }
        }
        self.last_payment_attempt = Some(std::time::Instant::now());

        let session = self.session.as_ref().ok_or(V1ClientError::NoSession)?;

        let step_size = session.step_size;
        let pricing = &session.selected_pricing;

        let preferred_steps = self
            .config
            .preferred_allotment
            .checked_div(step_size)
            .unwrap_or(1);
        let balance = wallet.balance().await.map_err(V1ClientError::Wallet)?;
        let max_affordable = balance.checked_div(pricing.price_per_step).unwrap_or(0);

        // Same funds-loss guard as connect(): a partial balance below the
        // minimum would mint an undersized token the gateway redeems and
        // then rejects with InsufficientSteps.
        if max_affordable < pricing.min_steps {
            return Err(V1ClientError::Pricing(
                pricing::PricingError::InsufficientFunds {
                    required: pricing.min_steps * pricing.price_per_step,
                    available: balance,
                },
            ));
        }

        let steps = preferred_steps
            .max(pricing.min_steps)
            .min(max_affordable)
            .max(1);

        let payment_amount = steps * pricing.price_per_step;
        tracing::info!(steps, payment_amount, "Renewing session");

        let token_bytes = wallet
            .create_token_with_overpayment(payment_amount, &pricing.mint_url, 100)
            .await
            .map_err(V1ClientError::Wallet)?;
        let token_str = String::from_utf8_lossy(&token_bytes).to_string();

        let session_event = self
            .submit_payment_with_recovery(
                wallet.as_ref(),
                &token_bytes,
                &token_str,
                &pricing.mint_url,
            )
            .await?;
        let allotment = session_event.allotment().unwrap_or(0);

        if let Some(s) = &mut self.session {
            s.total_allotment = allotment;
            s.session_event = Some(session_event);
        }

        tracing::info!(allotment, "Session renewed");
        Ok(())
    }

    /// Run the client loop: connect, poll usage, auto-renew.
    pub async fn run<W: V1ClientWallet + 'static>(
        &mut self,
        wallet: Arc<W>,
    ) -> Result<(), V1ClientError> {
        self.connect(&wallet).await?;

        let poll_interval = tokio::time::Duration::from_secs(self.config.poll_interval_secs);

        loop {
            tokio::select! {
                () = tokio::time::sleep(poll_interval) => {}
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Received shutdown signal, stopping client...");
                    return Ok(());
                }
            }

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

#[cfg(test)]
mod guard_tests {
    use super::*;
    use axum::Router;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use nostr::event::tag::{Tag, TagKind};
    use nostr::prelude::*;
    use std::sync::Mutex;

    /// Offline wallet mock: create debits and encodes the amount in the
    /// first 8 token bytes; receive credits it back.
    struct MockWallet {
        balance: Mutex<u64>,
    }

    impl MockWallet {
        fn new(balance: u64) -> Self {
            Self {
                balance: Mutex::new(balance),
            }
        }
    }

    impl V1ClientWallet for MockWallet {
        fn balance(&self) -> impl Future<Output = anyhow::Result<u64>> + Send {
            let balance = *self.balance.lock().unwrap();
            async move { Ok(balance) }
        }

        fn create_token_with_overpayment(
            &self,
            amount: u64,
            _mint_url: &str,
            _max_overpayment_absolute: u64,
        ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
            let result = {
                let mut balance = self.balance.lock().unwrap();
                if *balance < amount {
                    Err(anyhow::anyhow!("insufficient balance"))
                } else {
                    *balance -= amount;
                    Ok(amount.to_be_bytes().to_vec())
                }
            };
            async move { result }
        }

        fn receive_token(&self, token: &[u8]) -> impl Future<Output = anyhow::Result<u64>> + Send {
            let result = if token.len() < 8 {
                Err(anyhow::anyhow!("token too short"))
            } else {
                let amount =
                    u64::from_be_bytes(token[..8].try_into().expect("checked length above"));
                *self.balance.lock().unwrap() += amount;
                Ok(amount)
            };
            async move { result }
        }
    }

    /// Auto-recovery always fails — forces the file-fallback path.
    struct RejectReceiveWallet {
        inner: MockWallet,
    }

    impl V1ClientWallet for RejectReceiveWallet {
        fn balance(&self) -> impl Future<Output = anyhow::Result<u64>> + Send {
            self.inner.balance()
        }

        fn create_token_with_overpayment(
            &self,
            amount: u64,
            mint_url: &str,
            max_overpayment_absolute: u64,
        ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
            self.inner
                .create_token_with_overpayment(amount, mint_url, max_overpayment_absolute)
        }

        #[allow(clippy::manual_async_fn)]
        fn receive_token(&self, _token: &[u8]) -> impl Future<Output = anyhow::Result<u64>> + Send {
            async { Err(anyhow::anyhow!("auto-recovery disabled in test")) }
        }
    }

    async fn ad_event(State(state): State<Arc<Mutex<TestServer>>>) -> impl IntoResponse {
        let (keys, min_steps) = {
            let s = state.lock().unwrap();
            (s.keys.clone(), s.min_steps)
        };
        let tags = Tags::from_list(vec![
            Tag::custom(
                TagKind::Custom("metric".into()),
                ["milliseconds".to_owned()],
            ),
            Tag::custom(TagKind::Custom("step_size".into()), ["60000".to_owned()]),
            Tag::custom(
                TagKind::Custom("price_per_step".into()),
                [
                    "cashu".to_owned(),
                    "1".to_owned(),
                    "sat".to_owned(),
                    "https://testnut.cashu.exchange".to_owned(),
                    min_steps.to_string(),
                ],
            ),
        ]);
        let event = EventBuilder::new(Kind::Custom(10_021), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .expect("sign ad event");
        axum::Json(serde_json::to_value(event).expect("serialize ad event"))
    }

    async fn accepted_payment(State(state): State<Arc<Mutex<TestServer>>>) -> impl IntoResponse {
        let keys = {
            let mut s = state.lock().unwrap();
            s.payments_received += 1;
            s.allotment += 60_000;
            s.keys.clone()
        };
        let tags = Tags::from_list(vec![
            Tag::custom(TagKind::Custom("allotment".into()), ["60000".to_owned()]),
            Tag::custom(
                TagKind::Custom("metric".into()),
                ["milliseconds".to_owned()],
            ),
        ]);
        let event = EventBuilder::new(Kind::Custom(1022), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .expect("sign session event");
        axum::Json(serde_json::to_value(event).expect("serialize session event"))
    }

    async fn rejected_payment() -> impl IntoResponse {
        (
            axum::http::StatusCode::BAD_GATEWAY,
            "upstream rejected payment",
        )
    }

    async fn no_session_usage() -> impl IntoResponse {
        "0/0".to_owned()
    }

    async fn existing_session_usage() -> impl IntoResponse {
        "1000/60000".to_owned()
    }

    struct TestServer {
        keys: Keys,
        min_steps: u64,
        payments_received: u64,
        allotment: i64,
    }

    async fn start_server(
        state: Arc<Mutex<TestServer>>,
        reject_payment: bool,
        existing_session: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let usage_handler = if existing_session {
            get(existing_session_usage)
        } else {
            get(no_session_usage)
        };
        let payment_handler = if reject_payment {
            get(ad_event).post(rejected_payment)
        } else {
            get(ad_event).post(accepted_payment)
        };
        let app = Router::new()
            .route("/", payment_handler)
            .route("/usage", usage_handler)
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to random port");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server error");
        });
        (base_url, handle)
    }

    fn test_state(min_steps: u64) -> Arc<Mutex<TestServer>> {
        Arc::new(Mutex::new(TestServer {
            keys: Keys::generate(),
            min_steps,
            payments_received: 0,
            allotment: 0,
        }))
    }

    fn make_config() -> V1ClientConfig {
        V1ClientConfig {
            gateway_ip: "127.0.0.1".to_owned(),
            mac_address: "00:11:22:33:44:55".to_owned(),
            our_mint_urls: vec!["https://testnut.cashu.exchange".to_owned()],
            unit: "sat".to_owned(),
            max_price_per_ms: 1.0,
            max_price_per_byte: 1.0,
            preferred_allotment: 60_000,
            poll_interval_secs: 1,
            renewal_threshold: 0.8,
            recovery_file: std::path::PathBuf::from("/tmp/tollgate-v1c-test-recovery.txt"),
        }
    }

    fn test_client(base_url: &str) -> V1Client {
        V1Client::new_with_base_url(make_config(), base_url)
    }

    #[tokio::test]
    async fn reclaims_token_when_payment_submission_fails() {
        let state = test_state(1);
        let (base_url, server) = start_server(state.clone(), true, false).await;

        let wallet = Arc::new(MockWallet::new(1000));
        let mut client = test_client(&base_url);
        let result = client.connect(&wallet).await;
        assert!(
            result.is_err(),
            "connect must fail when payment is rejected"
        );

        let balance = wallet.balance().await.unwrap();
        assert_eq!(
            balance, 1000,
            "payment must be auto-reclaimed after a failed submission"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn persists_unrecoverable_token_to_recovery_file() {
        let state = test_state(1);
        let (base_url, server) = start_server(state, true, false).await;

        let dir = std::env::temp_dir().join(format!("tg-v1c-recover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let recovery_file = dir.join("tokens-to-recover.txt");

        let wallet = Arc::new(RejectReceiveWallet {
            inner: MockWallet::new(1000),
        });
        let mut config = make_config();
        config.recovery_file = recovery_file.clone();
        let mut client = V1Client::new_with_base_url(config, &base_url);
        let result = client.connect(&wallet).await;
        assert!(result.is_err());

        let contents = std::fs::read_to_string(&recovery_file)
            .expect("recovery file must exist when auto-reclaim fails");
        assert!(
            contents.contains("mint=https://testnut.cashu.exchange"),
            "{contents}"
        );
        assert!(contents.contains("token="), "{contents}");
        std::fs::remove_dir_all(&dir).ok();

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn reattach_rejects_pricing_above_budget_cap() {
        let state = test_state(1);
        let (base_url, server) = start_server(state, false, true).await;

        let wallet = Arc::new(MockWallet::new(1000));
        let mut config = make_config();
        // The ad prices 1 sat per 60_000 ms step ≈ 1.67e-5 sat/ms — cap
        // below that so the reattached pricing violates the budget.
        config.max_price_per_ms = 1e-6;
        let mut client = V1Client::new_with_base_url(config, &base_url);

        let result = client.connect(&wallet).await;
        assert!(
            result.is_err(),
            "reattaching to over-cap pricing must fail, not adopt it silently: {result:?}"
        );
        assert!(client.session().is_none());

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn reattach_accepts_pricing_within_budget() {
        let state = test_state(1);
        let (base_url, server) = start_server(state, false, true).await;

        let wallet = Arc::new(MockWallet::new(1000));
        let mut client = test_client(&base_url);
        client
            .connect(&wallet)
            .await
            .expect("reattach within budget must succeed");
        let session = client.session().expect("session adopted");
        assert_eq!(session.total_allotment, 60_000);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn rejects_purchase_below_minimum_steps() {
        // min_steps=10: a 5-sat wallet affords 5 steps — the pre-fix code
        // minted a 5-sat token the gateway redeems then rejects.
        let state = test_state(10);
        let (base_url, server) = start_server(state.clone(), false, false).await;

        let wallet = Arc::new(MockWallet::new(5));
        let mut client = test_client(&base_url);
        let result = client.connect(&wallet).await;
        assert!(
            result.is_err(),
            "a balance below the advertised minimum must be rejected: {result:?}"
        );
        let err = result.expect_err("checked is_err above").to_string();
        assert!(err.contains("insufficient funds"), "{err}");
        assert!(client.session().is_none());
        assert_eq!(
            state.lock().unwrap().payments_received,
            0,
            "no token may be submitted for a below-minimum purchase"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn renew_rejects_purchase_below_minimum_steps() {
        // 15 sats affords the 10-step minimum for connect (10-sat token),
        // leaving 5 — below the minimum for a renewal.
        let state = test_state(10);
        let (base_url, server) = start_server(state, false, false).await;

        let wallet = Arc::new(MockWallet::new(15));
        let mut client = test_client(&base_url);
        client
            .connect(&wallet)
            .await
            .expect("connect with 15 sats must afford the 10-step minimum");

        let result = client.renew(&wallet).await;
        assert!(
            result.is_err(),
            "renewal with a below-minimum balance must be rejected: {result:?}"
        );
        let err = result.expect_err("checked is_err above").to_string();
        assert!(err.contains("insufficient funds"), "{err}");

        server.abort();
        let _ = server.await;
    }
}
