//! In-memory store for Lightning invoice quote tracking.
//!
//! Tracks pending Lightning payment quotes and their lifecycle:
//! created (UNPAID) -> paid (PAID) -> minted (ISSUED) -> cleaned up.
//!
//! Also provides background tasks:
//! - [`spawn_quote_monitor`] — polls a single quote until paid, then grants access.
//! - [`spawn_quote_janitor`] — periodically removes stale quotes.
//!
//! Ported from the experimental v1 archive into the v1-compat layer.
//! All experimental `tollgate_core` and `super::*` dependencies have been
//! replaced with locally-defined types so this module manages LN quotes
//! independently.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// QuoteState — payment lifecycle for a Lightning invoice quote
// ---------------------------------------------------------------------------

/// Payment state of a Lightning invoice quote, mirroring the CDK mint quote
/// lifecycle: created (UNPAID) → paid (PAID) → minted (ISSUED).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteState {
    /// Quote has been created but the invoice has not been paid yet.
    Unpaid,
    /// Invoice has been paid; tokens can now be minted.
    Paid,
    /// Tokens have been minted and the quote is fully settled.
    Issued,
}

impl std::fmt::Display for QuoteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unpaid => write!(f, "UNPAID"),
            Self::Paid => write!(f, "PAID"),
            Self::Issued => write!(f, "ISSUED"),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum QuoteStoreError {
    #[error("internal error: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// LightningQuoteRecord
// ---------------------------------------------------------------------------

/// A single Lightning invoice quote record tracked by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightningQuoteRecord {
    /// CDK mint quote ID.
    pub quote_id: String,
    /// MAC address of the client that initiated the quote.
    pub mac_address: String,
    /// Cashu mint URL backing this quote.
    pub mint_url: String,
    /// Amount in sats.
    pub amount: u64,
    /// Invoice expiry as a unix timestamp.
    pub expiry: i64,
    /// Allotment granted after payment (0 until settled).
    pub allotment: u64,
    /// Creation time as a unix timestamp.
    pub created_at: i64,
    /// Time the quote was fully processed, if ever.
    pub completed_at: Option<i64>,
    /// Whether a session has been granted for this quote.
    pub session_granted: bool,
    /// In-flight flag: prevents duplicate mint between monitor and GET handler.
    pub processing: bool,
    /// BOLT11 invoice string (lnbc...).
    pub invoice: String,
    /// Last-known payment status, cached to avoid redundant mint API calls.
    pub cached_state: Option<QuoteState>,
    /// When `cached_state` was last updated (unix timestamp).
    pub cached_state_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// LightningQuoteStore trait (object-safe via boxed futures)
// ---------------------------------------------------------------------------

/// Alias for a boxed, sendable future returned by store methods.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Persistent or in-memory store for Lightning invoice quote records.
///
/// All methods are object-safe (returning boxed futures) so the trait can be
/// used as `Arc<dyn LightningQuoteStore>`.
pub trait LightningQuoteStore: Send + Sync {
    /// Insert a new quote record.
    fn insert(&self, record: LightningQuoteRecord) -> BoxFuture<'_, Result<(), QuoteStoreError>>;
    /// Look up a quote by ID.
    fn get(&self, quote_id: &str) -> BoxFuture<'_, Result<Option<LightningQuoteRecord>, QuoteStoreError>>;
    /// Look up a quote by ID, verifying the MAC address matches.
    fn get_for_mac(
        &self,
        quote_id: &str,
        mac: &str,
    ) -> BoxFuture<'_, Result<Option<LightningQuoteRecord>, QuoteStoreError>>;
    /// Replace a quote record (upsert).
    fn update(
        &self,
        quote_id: &str,
        record: LightningQuoteRecord,
    ) -> BoxFuture<'_, Result<(), QuoteStoreError>>;
    /// Remove a quote by ID.
    fn remove(&self, quote_id: &str) -> BoxFuture<'_, Result<(), QuoteStoreError>>;
    /// Return all quotes that are considered stale given `max_age_secs` and `now`.
    fn list_stale(
        &self,
        max_age_secs: i64,
        now: i64,
    ) -> BoxFuture<'_, Result<Vec<LightningQuoteRecord>, QuoteStoreError>>;
    /// Return all quotes whose cached state is Paid or Issued but not yet
    /// processed (not `processing`, not `session_granted`).
    fn list_paid_unprocessed(&self) -> BoxFuture<'_, Result<Vec<LightningQuoteRecord>, QuoteStoreError>>;
}

// ---------------------------------------------------------------------------
// InMemoryLightningQuoteStore
// ---------------------------------------------------------------------------

/// Simple in-memory implementation backed by a `tokio::sync::Mutex<HashMap>`.
#[derive(Default)]
pub struct InMemoryLightningQuoteStore {
    map: Mutex<HashMap<String, LightningQuoteRecord>>,
}

impl InMemoryLightningQuoteStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LightningQuoteStore for InMemoryLightningQuoteStore {
    fn insert(&self, record: LightningQuoteRecord) -> BoxFuture<'_, Result<(), QuoteStoreError>> {
        Box::pin(async move {
            let mut map = self.map.lock().await;
            map.insert(record.quote_id.clone(), record);
            Ok(())
        })
    }

    fn get(&self, quote_id: &str) -> BoxFuture<'_, Result<Option<LightningQuoteRecord>, QuoteStoreError>> {
        let quote_id = quote_id.to_owned();
        Box::pin(async move {
            let map = self.map.lock().await;
            Ok(map.get(&quote_id).cloned())
        })
    }

    fn get_for_mac(
        &self,
        quote_id: &str,
        mac: &str,
    ) -> BoxFuture<'_, Result<Option<LightningQuoteRecord>, QuoteStoreError>> {
        let quote_id = quote_id.to_owned();
        let mac = mac.to_owned();
        Box::pin(async move {
            let map = self.map.lock().await;
            Ok(match map.get(&quote_id) {
                Some(record) if record.mac_address == mac => Some(record.clone()),
                _ => None,
            })
        })
    }

    fn update(
        &self,
        quote_id: &str,
        record: LightningQuoteRecord,
    ) -> BoxFuture<'_, Result<(), QuoteStoreError>> {
        let quote_id = quote_id.to_owned();
        Box::pin(async move {
            let mut map = self.map.lock().await;
            map.insert(quote_id, record);
            Ok(())
        })
    }

    fn remove(&self, quote_id: &str) -> BoxFuture<'_, Result<(), QuoteStoreError>> {
        let quote_id = quote_id.to_owned();
        Box::pin(async move {
            let mut map = self.map.lock().await;
            map.remove(&quote_id);
            Ok(())
        })
    }

    fn list_stale(
        &self,
        max_age_secs: i64,
        now: i64,
    ) -> BoxFuture<'_, Result<Vec<LightningQuoteRecord>, QuoteStoreError>> {
        Box::pin(async move {
            let map = self.map.lock().await;
            let stale: Vec<LightningQuoteRecord> = map
                .values()
                .filter(|r| {
                    // Not processing AND NOT session_granted AND (now - created_at > max_age_secs)
                    if !r.processing && !r.session_granted {
                        let age = now - r.created_at;
                        if age > max_age_secs {
                            return true;
                        }
                    }
                    // OR session_granted AND completed_at is Some AND (now - completed_at > 600) (10min settled retention)
                    if r.session_granted {
                        if let Some(completed) = r.completed_at {
                            let completed_age = now - completed;
                            if completed_age > 600 {
                                return true;
                            }
                        }
                    }
                    // OR expiry > 0 AND now > expiry + 300 (5min grace past expiry)
                    if r.expiry > 0 && now > r.expiry + 300 {
                        return true;
                    }
                    false
                })
                .cloned()
                .collect();
            Ok(stale)
        })
    }

    fn list_paid_unprocessed(&self) -> BoxFuture<'_, Result<Vec<LightningQuoteRecord>, QuoteStoreError>> {
        Box::pin(async move {
            let map = self.map.lock().await;
            let unpaid_unprocessed: Vec<LightningQuoteRecord> = map
                .values()
                .filter(|r| {
                    !r.processing
                        && !r.session_granted
                        && (r.cached_state == Some(QuoteState::Paid)
                            || r.cached_state == Some(QuoteState::Issued))
                })
                .cloned()
                .collect();
            Ok(unpaid_unprocessed)
        })
    }
}

// ---------------------------------------------------------------------------
// QuoteProcessor trait — abstracts wallet/session/valve operations for the monitor
// ---------------------------------------------------------------------------

/// External operations needed by [`spawn_quote_monitor`] to poll quote status
/// and grant access when a payment is received.
///
/// This trait abstracts the CDK wallet, session store, and valve operations so
/// the monitor can be compiled without depending on a specific server state type.
/// The error type is [`String`] to keep the trait self-contained.
pub trait QuoteProcessor: Send + Sync {
    /// Check the current payment status of a Lightning quote.
    fn check_status(&self, quote_id: &str) -> BoxFuture<'_, Result<QuoteState, String>>;

    /// Mint tokens for a paid quote (NUT-04 mint operation).
    fn mint_tokens(&self, quote_id: &str) -> BoxFuture<'_, Result<(), String>>;

    /// Grant access for a settled quote: calculate allotment, create or update
    /// the customer session, and open the valve. Returns the calculated allotment.
    fn grant_access(&self, record: &LightningQuoteRecord) -> BoxFuture<'_, Result<u64, String>>;
}

// ---------------------------------------------------------------------------
// spawn_quote_monitor
// ---------------------------------------------------------------------------

/// Spawns a background task that monitors a single Lightning quote.
///
/// Polls [`QuoteProcessor::check_status`] every 2 seconds. When the quote
/// transitions to PAID or ISSUED, the monitor grants access: mints tokens (if
/// PAID), then delegates to [`QuoteProcessor::grant_access`] which calculates
/// the allotment, creates/updates the customer session, and opens the valve.
///
/// The task exits when: the quote is granted, the quote is removed from the
/// store, or 30 minutes have elapsed.
#[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
pub fn spawn_quote_monitor(
    quote_id: String,
    store: Arc<dyn LightningQuoteStore>,
    processor: Arc<dyn QuoteProcessor>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let poll_interval = Duration::from_secs(2);
        let max_lifetime = Duration::from_secs(30 * 60);
        let start = SystemTime::now();

        loop {
            tokio::time::sleep(poll_interval).await;

            if SystemTime::now()
                .duration_since(start)
                .unwrap_or(max_lifetime)
                >= max_lifetime
            {
                tracing::info!("quote monitor: expired after 30 min for {quote_id}");
                return;
            }

            let mut record = match store.get(&quote_id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    tracing::info!("quote monitor: quote removed, exiting for {quote_id}");
                    return;
                }
                Err(e) => {
                    tracing::warn!("quote monitor: store error for {quote_id}: {e}");
                    continue;
                }
            };

            if record.session_granted {
                tracing::info!("quote monitor: already granted, exiting for {quote_id}");
                return;
            }

            if record.processing {
                continue;
            }

            let quote_state = match processor.check_status(&quote_id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("quote monitor: status check failed for {quote_id}: {e}");
                    continue;
                }
            };

            record.cached_state = Some(quote_state);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            record.cached_state_at = Some(now);

            if quote_state != QuoteState::Paid && quote_state != QuoteState::Issued {
                let _ = store.update(&quote_id, record).await;
                continue;
            }

            record.processing = true;
            let _ = store.update(&quote_id, record.clone()).await;

            if quote_state == QuoteState::Paid {
                if let Err(e) = processor.mint_tokens(&quote_id).await {
                    tracing::error!("quote monitor: mint_tokens failed for {quote_id}: {e}");
                    record.processing = false;
                    let _ = store.update(&quote_id, record).await;
                    continue;
                }
            }

            let allotment = match processor.grant_access(&record).await {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("quote monitor: grant_access failed for {quote_id}: {e}");
                    record.processing = false;
                    let _ = store.update(&quote_id, record).await;
                    continue;
                }
            };

            record.session_granted = true;
            record.completed_at = Some(now);
            record.allotment = allotment;
            record.cached_state = Some(QuoteState::Issued);
            let _ = store.update(&quote_id, record).await;

            tracing::info!(
                "quote monitor: granted access for {quote_id} allotment={allotment}"
            );
            return;
        }
    })
}

// ---------------------------------------------------------------------------
// spawn_quote_janitor
// ---------------------------------------------------------------------------

/// Spawns a background task that periodically removes stale Lightning quotes.
///
/// Runs every `interval`. Queries [`LightningQuoteStore::list_stale`] with a
/// 30-minute max age and removes each stale quote.
#[allow(clippy::cast_possible_wrap)]
pub fn spawn_quote_janitor(
    quotes: Arc<dyn LightningQuoteStore>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let max_age_secs = 1800;

        loop {
            tokio::time::sleep(interval).await;

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            let stale = match quotes.list_stale(max_age_secs, now).await {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!("quote janitor: failed to list stale quotes: {e}");
                    continue;
                }
            };

            for quote in stale {
                match quotes.remove(&quote.quote_id).await {
                    Ok(()) => {
                        tracing::info!(
                            "quote janitor: removed stale quote_id={} mac={}",
                            quote.quote_id,
                            quote.mac_address,
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "quote janitor: failed to remove quote_id={}: {e}",
                            quote.quote_id,
                        );
                    }
                }
            }
        }
    })
}
