//! In-memory session tracking for the v1 HTTP/JSON server.
//!
//! Sessions are keyed by lowercase MAC address.  A session is created when a
//! payment is accepted and removed when the allotment is exhausted.  All
//! balance and usage calculations happen at query time from `start_time`.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

/// A single client session, created on successful payment.
#[derive(Debug, Clone)]
pub struct V1Session {
    /// Lowercase MAC address of the client (used as the session key).
    #[allow(dead_code)]
    pub mac: String,
    /// Metering metric: `"milliseconds"` (time-based) or `"bytes"` (data-based).
    pub metric: String,
    /// Unix timestamp (seconds) when the session started.
    pub start_time: i64,
    /// Total allotted units (milliseconds or bytes).
    pub allotment: u64,
    /// Total sats paid across all top-ups for this session.
    pub paid_amount: u64,
}

impl V1Session {
    /// Elapsed milliseconds since `start_time`.
    pub fn elapsed_ms(&self) -> i64 {
        let now = now_unix();
        (now - self.start_time).max(0) * 1000
    }

    /// Remaining milliseconds, clamped to 0.  Only meaningful for time-based
    /// sessions (`metric == "milliseconds"`).
    #[allow(dead_code)]
    pub fn remaining_ms(&self) -> i64 {
        let elapsed = self.elapsed_ms();
        (self.allotment as i64 - elapsed).max(0)
    }

    /// Whether the session has been fully consumed.
    #[allow(dead_code)]
    pub fn is_expired(&self) -> bool {
        if self.metric == "milliseconds" {
            self.remaining_ms() <= 0
        } else {
            // Bytes-based expiry requires counter reads; for now, not expired
            // unless the byte allotment is zero.
            self.allotment == 0
        }
    }
}

/// Thread-safe in-memory session store.
pub struct V1SessionStore {
    inner: Mutex<HashMap<String, V1Session>>,
}

impl V1SessionStore {
    /// Create an empty session store.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Look up a session by lowercase MAC.
    pub async fn get(&self, mac: &str) -> Option<V1Session> {
        self.inner.lock().await.get(mac).cloned()
    }

    /// Insert or replace a session, keyed by `session.mac.to_lowercase()`.
    #[allow(dead_code)]
    pub async fn insert(&self, session: V1Session) {
        let key = session.mac.to_lowercase();
        self.inner.lock().await.insert(key, session);
    }

    /// Add `extra_ms` to an existing session's allotment, or create a new one
    /// if it does not exist.  Returns the updated session.
    pub async fn top_up(
        &self,
        mac: &str,
        metric: &str,
        extra_ms: u64,
        paid_sats: u64,
    ) -> V1Session {
        let key = mac.to_lowercase();
        let mut map = self.inner.lock().await;
        let session = map.entry(key.clone()).or_insert(V1Session {
            mac: key,
            metric: metric.to_string(),
            start_time: now_unix(),
            allotment: 0,
            paid_amount: 0,
        });
        session.allotment = session.allotment.saturating_add(extra_ms);
        session.paid_amount = session.paid_amount.saturating_add(paid_sats);
        session.clone()
    }

    /// Remove a session, returning it if present.
    pub async fn remove(&self, mac: &str) -> Option<V1Session> {
        let key = mac.to_lowercase();
        self.inner.lock().await.remove(&key)
    }
}

impl Default for V1SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Current Unix timestamp in seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn top_up_creates_session() {
        let store = V1SessionStore::new();
        let session = store
            .top_up("AA:BB:CC:DD:EE:FF", "milliseconds", 60_000, 1)
            .await;
        assert_eq!(session.mac, "aa:bb:cc:dd:ee:ff");
        assert_eq!(session.allotment, 60_000);
        assert_eq!(session.paid_amount, 1);
    }

    #[tokio::test]
    async fn top_up_accumulates_allotment() {
        let store = V1SessionStore::new();
        store
            .top_up("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, 1)
            .await;
        let session = store
            .top_up("AA:BB:CC:DD:EE:FF", "milliseconds", 60_000, 1)
            .await;
        assert_eq!(session.allotment, 120_000);
        assert_eq!(session.paid_amount, 2);
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown_mac() {
        let store = V1SessionStore::new();
        assert!(store.get("00:00:00:00:00:00").await.is_none());
    }

    #[tokio::test]
    async fn remove_deletes_session() {
        let store = V1SessionStore::new();
        store
            .top_up("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, 1)
            .await;
        assert!(store.remove("aa:bb:cc:dd:ee:ff").await.is_some());
        assert!(store.get("aa:bb:cc:dd:ee:ff").await.is_none());
    }

    #[test]
    fn remaining_ms_decreases_over_time() {
        let session = V1Session {
            mac: "aa:bb:cc:dd:ee:ff".into(),
            metric: "milliseconds".into(),
            start_time: now_unix() - 10, // 10 seconds ago
            allotment: 60_000,           // 60 seconds
            paid_amount: 1,
        };
        // 10 seconds elapsed → 10_000 ms used, 50_000 remaining.
        let remaining = session.remaining_ms();
        assert!(remaining <= 50_000 && remaining > 49_000, "got {remaining}");
    }

    #[test]
    fn expired_session_has_zero_remaining() {
        let session = V1Session {
            mac: "aa:bb:cc:dd:ee:ff".into(),
            metric: "milliseconds".into(),
            start_time: now_unix() - 120, // 2 minutes ago
            allotment: 60_000,            // 1 minute
            paid_amount: 1,
        };
        assert!(session.is_expired());
        assert_eq!(session.remaining_ms(), 0);
    }
}
