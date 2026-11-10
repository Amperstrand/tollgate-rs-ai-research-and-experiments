//! In-memory store for Lightning invoice quote tracking.
//!
//! Tracks pending Lightning payment quotes and their lifecycle:
//! created (UNPAID) → paid (PAID) → minted (ISSUED) → cleaned up.
//!
//! Also provides background tasks:
//! - [`spawn_quote_monitor`] — polls a single quote until paid, then grants access.
//! - [`spawn_quote_janitor`] — periodically removes stale quotes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tollgate_core::wallet::Wallet;

use super::merchant;
use super::mint_quote_wallet::QuoteState;
use super::{CustomerSession, ServerState};

#[derive(Debug, thiserror::Error)]
pub enum QuoteStoreError {
    #[error("internal error: {0}")]
    Other(String),
}

#[async_trait]
pub trait LightningQuoteStore: Send + Sync {
    async fn insert(&self, record: LightningQuoteRecord) -> Result<(), QuoteStoreError>;
    async fn get(&self, quote_id: &str) -> Result<Option<LightningQuoteRecord>, QuoteStoreError>;
    async fn get_for_mac(
        &self,
        quote_id: &str,
        mac: &str,
    ) -> Result<Option<LightningQuoteRecord>, QuoteStoreError>;
    async fn update(
        &self,
        quote_id: &str,
        record: LightningQuoteRecord,
    ) -> Result<(), QuoteStoreError>;
    /// Atomically claim a paid-but-unprocessed quote for minting/granting.
    /// Exactly one concurrent caller gets the record; the others get `None`
    /// (another writer holds the claim, or the quote is already granted).
    async fn claim_for_processing(
        &self,
        quote_id: &str,
    ) -> Result<Option<LightningQuoteRecord>, QuoteStoreError>;
    /// Record the completed grant for a claimed quote.
    async fn mark_granted(
        &self,
        quote_id: &str,
        allotment: u64,
        now: i64,
    ) -> Result<(), QuoteStoreError>;
    /// Release a failed claim so a later poll can retry it (no-op when the
    /// quote has since been granted).
    async fn reset_processing(&self, quote_id: &str) -> Result<(), QuoteStoreError>;
    /// Refresh only the cached status snapshot — must never touch the
    /// processing/granted flags.
    async fn refresh_cached_state(
        &self,
        quote_id: &str,
        quote_state: QuoteState,
        at: i64,
    ) -> Result<(), QuoteStoreError>;
    async fn remove(&self, quote_id: &str) -> Result<(), QuoteStoreError>;
    async fn list_stale(
        &self,
        max_age_secs: i64,
        now: i64,
    ) -> Result<Vec<LightningQuoteRecord>, QuoteStoreError>;
    async fn list_paid_unprocessed(&self) -> Result<Vec<LightningQuoteRecord>, QuoteStoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightningQuoteRecord {
    pub quote_id: String,
    pub mac_address: String,
    pub mint_url: String,
    pub amount: u64,
    pub expiry: i64,     // unix timestamp
    pub allotment: u64,  // set after payment
    pub created_at: i64, // unix timestamp
    pub completed_at: Option<i64>,
    pub session_granted: bool,
    pub processing: bool, // prevents duplicate mint between monitor and GET handler
    pub invoice: String,  // BOLT11 invoice string
    pub cached_state: Option<QuoteState>,
    pub cached_state_at: Option<i64>, // unix timestamp
}

#[derive(Default)]
pub struct InMemoryLightningQuoteStore {
    map: Mutex<HashMap<String, LightningQuoteRecord>>,
}

impl InMemoryLightningQuoteStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LightningQuoteStore for InMemoryLightningQuoteStore {
    async fn insert(&self, record: LightningQuoteRecord) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        map.insert(record.quote_id.clone(), record);
        Ok(())
    }

    async fn get(&self, quote_id: &str) -> Result<Option<LightningQuoteRecord>, QuoteStoreError> {
        let map = self.map.lock().await;
        Ok(map.get(quote_id).cloned())
    }

    async fn get_for_mac(
        &self,
        quote_id: &str,
        mac: &str,
    ) -> Result<Option<LightningQuoteRecord>, QuoteStoreError> {
        let map = self.map.lock().await;
        Ok(match map.get(quote_id) {
            Some(record) if record.mac_address == mac => Some(record.clone()),
            _ => None,
        })
    }

    async fn update(
        &self,
        quote_id: &str,
        record: LightningQuoteRecord,
    ) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        map.insert(quote_id.to_owned(), record);
        Ok(())
    }

    async fn remove(&self, quote_id: &str) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        map.remove(quote_id);
        Ok(())
    }

    async fn claim_for_processing(
        &self,
        quote_id: &str,
    ) -> Result<Option<LightningQuoteRecord>, QuoteStoreError> {
        let mut map = self.map.lock().await;
        match map.get_mut(quote_id) {
            Some(record) if !record.session_granted && !record.processing => {
                record.processing = true;
                Ok(Some(record.clone()))
            }
            _ => Ok(None),
        }
    }

    async fn mark_granted(
        &self,
        quote_id: &str,
        allotment: u64,
        now: i64,
    ) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        if let Some(record) = map.get_mut(quote_id) {
            record.session_granted = true;
            record.processing = false;
            record.completed_at = Some(now);
            record.allotment = allotment;
            record.cached_state = Some(QuoteState::Issued);
        }
        Ok(())
    }

    async fn reset_processing(&self, quote_id: &str) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        if let Some(record) = map.get_mut(quote_id) {
            if !record.session_granted {
                record.processing = false;
            }
        }
        Ok(())
    }

    async fn refresh_cached_state(
        &self,
        quote_id: &str,
        quote_state: QuoteState,
        at: i64,
    ) -> Result<(), QuoteStoreError> {
        let mut map = self.map.lock().await;
        if let Some(record) = map.get_mut(quote_id) {
            record.cached_state = Some(quote_state);
            record.cached_state_at = Some(at);
        }
        Ok(())
    }

    async fn list_stale(
        &self,
        max_age_secs: i64,
        now: i64,
    ) -> Result<Vec<LightningQuoteRecord>, QuoteStoreError> {
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
    }

    async fn list_paid_unprocessed(&self) -> Result<Vec<LightningQuoteRecord>, QuoteStoreError> {
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
    }
}

/// Spawns a background task that monitors a single Lightning quote.
///
/// Polls `check_mint_quote_status` every 2 seconds. When the quote transitions
/// to PAID or ISSUED, the monitor grants access using the same flow as
/// `handle_get_ln_invoice`: mints tokens (if PAID), calculates allotment,
/// creates/updates the customer session, opens the valve, and marks the quote
/// as completed.
///
/// The task exits when: the quote is granted, the quote is removed from the
/// store, or 30 minutes have elapsed.
#[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
pub fn spawn_quote_monitor<W: Wallet + 'static>(
    quote_id: String,
    state: Arc<ServerState<W>>,
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

            let record = match state.lightning_quotes.get(&quote_id).await {
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

            let wallet = if let Some(w) = &state.mint_quote_wallet {
                Arc::clone(w)
            } else {
                tracing::warn!("quote monitor: no mint_quote_wallet configured");
                return;
            };

            let quote_state = match wallet.check_mint_quote_status(&quote_id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("quote monitor: status check failed for {quote_id}: {e}");
                    continue;
                }
            };

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            if quote_state != QuoteState::Paid && quote_state != QuoteState::Issued {
                let _ = state
                    .lightning_quotes
                    .refresh_cached_state(&quote_id, quote_state, now)
                    .await;
                continue;
            }

            let claimed = match state.lightning_quotes.claim_for_processing(&quote_id).await {
                Ok(Some(record)) => record,
                Ok(None) => {
                    // A concurrent GET poll owns the grant, or it is done.
                    continue;
                }
                Err(e) => {
                    tracing::warn!("quote monitor: claim failed for {quote_id}: {e}");
                    continue;
                }
            };

            if quote_state == QuoteState::Paid {
                if let Err(e) = wallet.mint_tokens(&quote_id).await {
                    tracing::error!("quote monitor: mint_tokens failed for {quote_id}: {e}");
                    let _ = state.lightning_quotes.reset_processing(&quote_id).await;
                    continue;
                }
            }

            let allotment = match merchant::calculate_allotment(
                claimed.amount,
                &claimed.mint_url,
                &state.config,
            ) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("quote monitor: allotment calc failed for {quote_id}: {e}");
                    let _ = state.lightning_quotes.reset_processing(&quote_id).await;
                    continue;
                }
            };

            let mac = claimed.mac_address.clone();
            let existing = state.sessions.get(&mac).await.ok().flatten();
            if let Some(mut s) = existing {
                super::handlers::apply_renewal(&mut s, allotment, now);
                let _ = state.sessions.update(&mac, s).await;
            } else {
                let s = CustomerSession {
                    mac_address: mac.clone(),
                    start_time: now,
                    metric: state.config.metric.clone(),
                    allotment,
                };
                let _ = state.sessions.insert(s).await;
            }

            if let Err(e) = state.valve.open_gate(&mac) {
                tracing::warn!("quote monitor: valve open failed for {mac}: {e}");
            }

            let _ = state
                .lightning_quotes
                .mark_granted(&quote_id, allotment, now)
                .await;

            tracing::info!(
                "quote monitor: granted access for {quote_id} mac={mac} allotment={allotment}"
            );
            return;
        }
    })
}

/// Spawns a background task that periodically removes stale Lightning quotes.
///
/// Runs every `interval`. Queries [`LightningQuoteStore::list_stale`] with a
/// 30-minute max age and removes each stale quote. Follows the pattern in
/// [`crate::v1::server::janitor::spawn_janitor`].
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_quote(
        quote_id: &str,
        mac: &str,
        mint_url: &str,
        amount: u64,
        created_at: i64,
        expiry: i64,
        cached_state: Option<QuoteState>,
    ) -> LightningQuoteRecord {
        LightningQuoteRecord {
            quote_id: quote_id.to_owned(),
            mac_address: mac.to_owned(),
            mint_url: mint_url.to_owned(),
            amount,
            expiry,
            allotment: 0,
            created_at,
            completed_at: None,
            session_granted: false,
            processing: false,
            invoice: format!("lnbc{amount}u1p3hxnylpp5qx..."),
            cached_state,
            cached_state_at: None,
        }
    }

    #[tokio::test]
    async fn claim_for_processing_grants_exactly_one_claim() {
        let store = InMemoryLightningQuoteStore::new();
        store
            .insert(make_quote(
                "q1",
                "aa:bb:cc:dd:ee:ff",
                "http://mint.example.com",
                1000,
                1000,
                2000,
                Some(QuoteState::Paid),
            ))
            .await
            .unwrap();

        let first = store.claim_for_processing("q1").await.unwrap();
        let second = store.claim_for_processing("q1").await.unwrap();

        assert!(first.is_some(), "first claimant wins the quote");
        assert!(second.is_none(), "second concurrent claimant is rejected");
    }

    #[tokio::test]
    async fn claim_rejected_after_grant() {
        let store = InMemoryLightningQuoteStore::new();
        store
            .insert(make_quote(
                "q1",
                "aa:bb:cc:dd:ee:ff",
                "http://mint.example.com",
                1000,
                1000,
                2000,
                Some(QuoteState::Issued),
            ))
            .await
            .unwrap();

        store.claim_for_processing("q1").await.unwrap().unwrap();
        store.mark_granted("q1", 6000, 1234).await.unwrap();

        assert!(
            store.claim_for_processing("q1").await.unwrap().is_none(),
            "a granted quote can never be claimed again"
        );

        let record = store.get("q1").await.unwrap().unwrap();
        assert!(record.session_granted);
        assert!(!record.processing);
        assert_eq!(record.allotment, 6000);
        assert_eq!(record.completed_at, Some(1234));
    }

    #[tokio::test]
    async fn reset_processing_ignored_once_granted() {
        let store = InMemoryLightningQuoteStore::new();
        store
            .insert(make_quote(
                "q1",
                "aa:bb:cc:dd:ee:ff",
                "http://mint.example.com",
                1000,
                1000,
                2000,
                Some(QuoteState::Paid),
            ))
            .await
            .unwrap();

        store.claim_for_processing("q1").await.unwrap().unwrap();
        store.mark_granted("q1", 6000, 1234).await.unwrap();

        // A late failure path must not resurrect a completed grant.
        store.reset_processing("q1").await.unwrap();
        let record = store.get("q1").await.unwrap().unwrap();
        assert!(record.session_granted, "grant state must survive reset");
    }

    #[tokio::test]
    async fn refresh_cached_state_preserves_flags() {
        let store = InMemoryLightningQuoteStore::new();
        store
            .insert(make_quote(
                "q1",
                "aa:bb:cc:dd:ee:ff",
                "http://mint.example.com",
                1000,
                1000,
                2000,
                Some(QuoteState::Unpaid),
            ))
            .await
            .unwrap();

        store.claim_for_processing("q1").await.unwrap().unwrap();
        store
            .refresh_cached_state("q1", QuoteState::Paid, 1500)
            .await
            .unwrap();

        let record = store.get("q1").await.unwrap().unwrap();
        assert_eq!(record.cached_state, Some(QuoteState::Paid));
        assert_eq!(record.cached_state_at, Some(1500));
        assert!(
            record.processing,
            "a stale status refresh must not clear the processing flag"
        );
        assert!(!record.session_granted);
    }

    #[tokio::test]
    async fn concurrent_claims_yield_exactly_one_winner() {
        let store = Arc::new(InMemoryLightningQuoteStore::new());
        store
            .insert(make_quote(
                "q1",
                "aa:bb:cc:dd:ee:ff",
                "http://mint.example.com",
                1000,
                1000,
                2000,
                Some(QuoteState::Paid),
            ))
            .await
            .unwrap();

        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut joins = Vec::new();
        for _ in 0..16 {
            let store = store.clone();
            let winners = winners.clone();
            joins.push(tokio::spawn(async move {
                if store.claim_for_processing("q1").await.unwrap().is_some() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for j in joins {
            j.await.unwrap();
        }

        assert_eq!(
            winners.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one of 16 concurrent claimants may win"
        );
    }

    #[tokio::test]
    async fn insert_and_get() {
        let store = InMemoryLightningQuoteStore::new();
        let record = make_quote(
            "abc123",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        store.insert(record.clone()).await.unwrap();

        let got = store.get("abc123").await.unwrap();
        assert_eq!(got, Some(record));

        let missing = store.get("xyz789").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn get_unknown_returns_none() {
        let store = InMemoryLightningQuoteStore::new();
        let missing = store.get("nonexistent").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn get_for_mac_matches() {
        let store = InMemoryLightningQuoteStore::new();
        let record = make_quote(
            "abc123",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        store.insert(record.clone()).await.unwrap();

        let got = store
            .get_for_mac("abc123", "aa:bb:cc:dd:ee:ff")
            .await
            .unwrap();
        assert_eq!(got, Some(record));

        let mismatch = store
            .get_for_mac("abc123", "11:22:33:44:55:66")
            .await
            .unwrap();
        assert_eq!(mismatch, None);
    }

    #[tokio::test]
    async fn get_for_mac_mismatch_returns_none() {
        let store = InMemoryLightningQuoteStore::new();
        let record = make_quote(
            "abc123",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        store.insert(record.clone()).await.unwrap();

        let mismatch = store
            .get_for_mac("abc123", "11:22:33:44:55:66")
            .await
            .unwrap();
        assert_eq!(mismatch, None);
    }

    #[tokio::test]
    async fn update_modifies_record() {
        let store = InMemoryLightningQuoteStore::new();
        let record = make_quote(
            "abc123",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        store.insert(record.clone()).await.unwrap();

        let mut updated = record;
        updated.session_granted = true;
        updated.completed_at = Some(2000);
        store.update("abc123", updated.clone()).await.unwrap();

        let got = store.get("abc123").await.unwrap().unwrap();
        assert!(got.session_granted);
        assert_eq!(got.completed_at, Some(2000));
    }

    #[tokio::test]
    async fn remove_deletes_record() {
        let store = InMemoryLightningQuoteStore::new();
        let record = make_quote(
            "abc123",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        store.insert(record.clone()).await.unwrap();

        store.remove("abc123").await.unwrap();

        let gone = store.get("abc123").await.unwrap();
        assert_eq!(gone, None);
    }

    #[tokio::test]
    async fn list_stale_by_age() {
        let store = InMemoryLightningQuoteStore::new();
        let now = 2000;

        let old1 = make_quote(
            "old1",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );
        let old2 = make_quote(
            "old2",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1400,
            2000,
            Some(QuoteState::Unpaid),
        );
        let old3 = make_quote(
            "old3",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1600,
            2000,
            Some(QuoteState::Unpaid),
        );
        let recent = make_quote(
            "recent",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1990,
            2000,
            Some(QuoteState::Unpaid),
        );

        store.insert(old1.clone()).await.unwrap();
        store.insert(old2.clone()).await.unwrap();
        store.insert(old3.clone()).await.unwrap();
        store.insert(recent.clone()).await.unwrap();

        let stale = store.list_stale(500, now).await.unwrap();
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().any(|r| r.quote_id == "old1"));
        assert!(stale.iter().any(|r| r.quote_id == "old2"));
    }

    #[tokio::test]
    async fn list_stale_keeps_recent() {
        let store = InMemoryLightningQuoteStore::new();
        let now = 2000;

        let recent = make_quote(
            "recent",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1990,
            2000,
            Some(QuoteState::Unpaid),
        );
        let very_recent = make_quote(
            "very_recent",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1995,
            2000,
            Some(QuoteState::Unpaid),
        );

        store.insert(recent.clone()).await.unwrap();
        store.insert(very_recent.clone()).await.unwrap();

        let stale = store.list_stale(500, now).await.unwrap();
        assert_eq!(stale.len(), 0);
    }

    #[tokio::test]
    async fn list_stale_settled_retention() {
        let store = InMemoryLightningQuoteStore::new();
        let now = 3000;

        let mut old_settled = make_quote(
            "old_settled",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Paid),
        );
        old_settled.session_granted = true;
        old_settled.completed_at = Some(2000);

        let mut recent_settled = make_quote(
            "recent_settled",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            2000,
            3000,
            Some(QuoteState::Paid),
        );
        recent_settled.session_granted = true;
        recent_settled.completed_at = Some(2500);

        store.insert(old_settled.clone()).await.unwrap();
        store.insert(recent_settled.clone()).await.unwrap();

        let stale = store.list_stale(500, now).await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].quote_id, "old_settled");
    }

    #[tokio::test]
    async fn list_paid_unprocessed() {
        let store = InMemoryLightningQuoteStore::new();

        let paid1 = make_quote(
            "paid1",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Paid),
        );
        let issued1 = make_quote(
            "issued1",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Issued),
        );

        let mut granted = make_quote(
            "granted",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Paid),
        );
        granted.session_granted = true;

        let mut processing = make_quote(
            "processing",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Paid),
        );
        processing.processing = true;

        store.insert(paid1.clone()).await.unwrap();
        store.insert(issued1.clone()).await.unwrap();
        store.insert(granted.clone()).await.unwrap();
        store.insert(processing.clone()).await.unwrap();

        let unprocessed = store.list_paid_unprocessed().await.unwrap();
        assert_eq!(unprocessed.len(), 2);
        assert!(unprocessed.iter().any(|r| r.quote_id == "paid1"));
        assert!(unprocessed.iter().any(|r| r.quote_id == "issued1"));
    }

    #[tokio::test]
    async fn list_paid_unprocessed_skips_granted() {
        let store = InMemoryLightningQuoteStore::new();

        let mut granted = make_quote(
            "granted",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Paid),
        );
        granted.session_granted = true;

        let unpaid = make_quote(
            "unpaid",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );

        store.insert(granted.clone()).await.unwrap();
        store.insert(unpaid.clone()).await.unwrap();

        let unprocessed = store.list_paid_unprocessed().await.unwrap();
        assert_eq!(unprocessed.len(), 0);
    }

    #[tokio::test]
    async fn list_paid_unprocessed_skips_unpaid() {
        let store = InMemoryLightningQuoteStore::new();

        let unpaid = make_quote(
            "unpaid",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Unpaid),
        );

        let issued = make_quote(
            "issued",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            1000,
            2000,
            Some(QuoteState::Issued),
        );

        store.insert(unpaid.clone()).await.unwrap();
        store.insert(issued.clone()).await.unwrap();

        let unprocessed = store.list_paid_unprocessed().await.unwrap();
        assert_eq!(unprocessed.len(), 1);
        assert_eq!(unprocessed[0].quote_id, "issued");
    }

    #[allow(clippy::cast_possible_wrap)]
    #[tokio::test]
    async fn janitor_removes_stale_quotes() {
        let store = Arc::new(InMemoryLightningQuoteStore::new());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let old_quote = make_quote(
            "stale1",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            now - 2000,
            now - 1000,
            Some(QuoteState::Unpaid),
        );
        let old_quote2 = make_quote(
            "stale2",
            "11:22:33:44:55:66",
            "http://mint.example.com",
            500,
            now - 2000,
            now - 1000,
            Some(QuoteState::Unpaid),
        );
        store.insert(old_quote).await.unwrap();
        store.insert(old_quote2).await.unwrap();

        let quotes: Arc<dyn LightningQuoteStore> = store.clone();
        let handle = spawn_quote_janitor(quotes, Duration::from_millis(50));

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("stale1").await.unwrap().is_none(),
            "stale quote should have been removed"
        );
        assert!(
            store.get("stale2").await.unwrap().is_none(),
            "stale quote should have been removed"
        );
    }

    #[allow(clippy::cast_possible_wrap)]
    #[tokio::test]
    async fn janitor_keeps_recent_quotes() {
        let store = Arc::new(InMemoryLightningQuoteStore::new());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let recent = make_quote(
            "recent1",
            "aa:bb:cc:dd:ee:ff",
            "http://mint.example.com",
            1000,
            now,
            now + 3600,
            Some(QuoteState::Unpaid),
        );
        store.insert(recent.clone()).await.unwrap();

        let quotes: Arc<dyn LightningQuoteStore> = store.clone();
        let handle = spawn_quote_janitor(quotes, Duration::from_millis(50));

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        let got = store.get("recent1").await.unwrap();
        assert!(got.is_some(), "recent quote should NOT have been removed");
        assert_eq!(got.unwrap().quote_id, "recent1");
    }

    // Monitor tests covered by integration tests in v1_api_parity.rs
}
