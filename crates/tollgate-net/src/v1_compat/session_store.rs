//! Session store for the v1_compat REST API.
//!
//! Provides persistent session state for the session_api endpoints
//! (tollgate-auth consumer). In-memory implementation; the SqliteSessionStore
//! from the m2 branch is deferred (libsqlite3-sys version conflict with
//! cdk-sqlite — both link the sqlite3 native library).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use super::merchant::{CustomerSession, ExternalUsageSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

/// Trait for session storage backends.
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    async fn get(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError>;
    async fn insert(&self, session: CustomerSession) -> Result<(), SessionStoreError>;
    async fn remove(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError>;
    async fn update(&self, mac: &str, session: CustomerSession) -> Result<(), SessionStoreError>;
    async fn list_all(&self) -> Result<Vec<CustomerSession>, SessionStoreError>;
    async fn list_expired(&self, now_secs: i64) -> Result<Vec<CustomerSession>, SessionStoreError>;

    /// Atomically add allotment to a session (or create if absent).
    /// The read-modify-write runs under the store's lock so overlapping
    /// payments cannot lose each other's top-ups.
    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError>;

    /// Atomically record an external usage snapshot without overwriting
    /// allotment (a concurrent renewal's write is preserved).
    async fn record_external_usage(
        &self,
        mac: &str,
        snapshot: ExternalUsageSnapshot,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;

    /// Evict only if the CURRENT record is still wall-clock expired.
    async fn remove_expired(
        &self,
        mac: &str,
        now_secs: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;

    /// Evict only if the CURRENT record's allotment is still exhausted.
    async fn remove_exhausted_bytes(
        &self,
        mac: &str,
        usage: u64,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;
}

/// Extend a session's allotment without restoring consumed time.
///
/// `now`/`start_time` are unix seconds while `allotment` is in the
/// session's metric units (milliseconds for time metrics). Non-time
/// metrics (bytes) are not consumed by wall-clock, so their start_time
/// resets.
pub(crate) fn apply_renewal(session: &mut CustomerSession, allotment: u64, now: i64) {
    let elapsed_secs = (now - session.start_time).max(0);
    let consumed_secs = if session.metric == "milliseconds" {
        (elapsed_secs * 1000).min(i64::try_from(session.allotment).unwrap_or(i64::MAX)) / 1000
    } else {
        0
    };
    session.allotment += allotment;
    session.start_time = now - consumed_secs;
}

/// In-memory session store (tokio Mutex<HashMap>).
#[derive(Default)]
pub struct InMemorySessionStore {
    map: Mutex<HashMap<String, CustomerSession>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn is_time_expired(s: &CustomerSession, now_secs: i64) -> bool {
    s.metric == "milliseconds" && (now_secs - s.start_time) * 1000 >= s.allotment as i64
}

#[async_trait::async_trait]
impl SessionStore for InMemorySessionStore {
    async fn get(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let map = self.map.lock().await;
        Ok(map.get(mac).cloned())
    }

    async fn insert(&self, session: CustomerSession) -> Result<(), SessionStoreError> {
        let mut map = self.map.lock().await;
        map.insert(session.mac_address.clone(), session);
        Ok(())
    }

    async fn remove(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let mut map = self.map.lock().await;
        Ok(map.remove(mac))
    }

    async fn update(&self, mac: &str, session: CustomerSession) -> Result<(), SessionStoreError> {
        let mut map = self.map.lock().await;
        map.insert(mac.to_owned(), session);
        Ok(())
    }

    async fn list_all(&self) -> Result<Vec<CustomerSession>, SessionStoreError> {
        let map = self.map.lock().await;
        Ok(map.values().cloned().collect())
    }

    async fn list_expired(&self, now_secs: i64) -> Result<Vec<CustomerSession>, SessionStoreError> {
        let map = self.map.lock().await;
        Ok(map
            .values()
            .filter(|s| is_time_expired(s, now_secs))
            .cloned()
            .collect())
    }

    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError> {
        let mut map = self.map.lock().await;
        match map.get_mut(mac) {
            Some(session) => {
                apply_renewal(session, amount, now);
                Ok(session.clone())
            }
            None => {
                let session = CustomerSession {
                    mac_address: mac.to_owned(),
                    start_time: now,
                    metric: metric.to_owned(),
                    allotment: amount,
                    last_external_usage: None,
                };
                map.insert(mac.to_owned(), session.clone());
                Ok(session)
            }
        }
    }

    async fn record_external_usage(
        &self,
        mac: &str,
        snapshot: ExternalUsageSnapshot,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let mut map = self.map.lock().await;
        Ok(match map.get_mut(mac) {
            Some(session) => {
                session.last_external_usage = Some(snapshot);
                Some(session.clone())
            }
            None => None,
        })
    }

    async fn remove_expired(
        &self,
        mac: &str,
        now_secs: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let mut map = self.map.lock().await;
        let still_expired = map.get(mac).is_some_and(|s| is_time_expired(s, now_secs));
        if still_expired {
            Ok(map.remove(mac))
        } else {
            Ok(None)
        }
    }

    async fn remove_exhausted_bytes(
        &self,
        mac: &str,
        usage: u64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let mut map = self.map.lock().await;
        let still_exhausted = map.get(mac).is_some_and(|s| usage >= s.allotment);
        if still_exhausted {
            Ok(map.remove(mac))
        } else {
            Ok(None)
        }
    }
}

/// Current unix time in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn add_allotment_creates_and_tops_up() {
        let store = InMemorySessionStore::new();

        let s1 = store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, 1000)
            .await
            .unwrap();
        assert_eq!(s1.allotment, 60_000);

        let s2 = store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 30_000, 1005)
            .await
            .unwrap();
        // apply_renewal adds to the TOTAL allotment (not remaining):
        // 60_000 + 30_000 = 90_000, with start_time adjusted to preserve
        // the 5s of consumed time (start_time = 1005 - 5 = 1000).
        assert_eq!(s2.allotment, 90_000);
        assert_eq!(s2.start_time, 1000);
    }

    #[tokio::test]
    async fn concurrent_topups_all_land() {
        let store = std::sync::Arc::new(InMemorySessionStore::new());

        let mut joins = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            joins.push(tokio::spawn(async move {
                store
                    .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 1_000, 5_000)
                    .await
                    .unwrap()
            }));
        }
        let mut results = Vec::new();
        for j in joins {
            results.push(j.await.unwrap().allotment);
        }
        let max = results.into_iter().max().unwrap();
        assert_eq!(max, 8_000, "every concurrent top-up must land");
    }

    #[tokio::test]
    async fn external_usage_preserves_allotment() {
        let store = InMemorySessionStore::new();
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 5_000, 10_000)
            .await
            .unwrap();

        let snapshot = ExternalUsageSnapshot {
            input_octets: 100,
            output_octets: 200,
            session_time: 5,
            reported_at: 11_000,
        };
        let updated = store
            .record_external_usage("aa:bb:cc:dd:ee:ff", snapshot.clone())
            .await
            .unwrap()
            .expect("session exists");
        assert_eq!(updated.allotment, 5_000);
        assert_eq!(updated.last_external_usage, Some(snapshot));
    }

    #[tokio::test]
    async fn remove_expired_spares_renewed() {
        let store = InMemorySessionStore::new();
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 1_000, 0)
            .await
            .unwrap();
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 3_600_000, 10_000)
            .await
            .unwrap();

        let removed = store
            .remove_expired("aa:bb:cc:dd:ee:ff", 10_005)
            .await
            .unwrap();
        assert!(
            removed.is_none(),
            "renewed session must survive stale eviction"
        );
    }
}
