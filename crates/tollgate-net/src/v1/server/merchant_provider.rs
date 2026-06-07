use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tollgate_core::wallet::Wallet;

use super::{CustomerSession, SessionStore};

/// Thread-safe wrapper allowing atomic swap of the wallet/merchant.
/// Matches Go's `MerchantProvider` pattern — RWMutex protecting a merchant interface.
/// Swaps are rare (once per mint recovery); reads happen on every HTTP request.
pub struct MerchantProvider {
    wallet: RwLock<Arc<dyn Wallet>>,
}

impl MerchantProvider {
    pub fn new(wallet: Arc<dyn Wallet>) -> Self {
        Self {
            wallet: RwLock::new(wallet),
        }
    }

    /// Returns a clone of the current wallet Arc.
    /// In-flight requests keep their reference alive after swap.
    pub fn get(&self) -> Arc<dyn Wallet> {
        self.wallet
            .read()
            .expect("merchant provider lock not poisoned")
            .clone()
    }

    /// Atomically swaps the wallet. In-flight requests on the old wallet continue uninterrupted.
    pub fn swap(&self, new_wallet: Arc<dyn Wallet>) {
        let mut guard = self
            .wallet
            .write()
            .expect("merchant provider lock not poisoned");
        *guard = new_wallet;
    }
}

/// Add allotment to an existing session or create a new one.
/// Mirrors Go v1's `Merchant.AddAllotment(macAddress, metric, amount)`.
///
/// If a session for `mac` already exists, its allotment is increased by
/// `allotment` and `start_time` is reset to now. Otherwise a new session
/// is created with the given `metric` and `allotment`.
pub async fn add_allotment(
    sessions: &dyn SessionStore,
    mac: &str,
    metric: &str,
    allotment: u64,
) -> Result<CustomerSession, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let existing = sessions.get(mac).await.map_err(|e| e.to_string())?;

    let session = if let Some(mut s) = existing {
        s.allotment += allotment;
        s.start_time = now;
        let updated = s.clone();
        sessions
            .update(mac, s)
            .await
            .map_err(|e| e.to_string())?;
        updated
    } else {
        let s = CustomerSession {
            mac_address: mac.to_owned(),
            start_time: now,
            metric: metric.to_owned(),
            allotment,
        };
        let cloned = s.clone();
        sessions.insert(s).await.map_err(|e| e.to_string())?;
        cloned
    };

    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::server::InMemorySessionStore;

    #[tokio::test]
    async fn test_add_allotment_creates_new_session() {
        let store = InMemorySessionStore::new();

        let session = add_allotment(&store, "aa:bb:cc:dd:ee:ff", "milliseconds", 60_000)
            .await
            .unwrap();

        assert_eq!(session.mac_address, "aa:bb:cc:dd:ee:ff");
        assert_eq!(session.metric, "milliseconds");
        assert_eq!(session.allotment, 60_000);
        assert!(session.start_time > 0);

        // Verify persisted
        let stored = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(stored.allotment, 60_000);
        assert_eq!(stored.metric, "milliseconds");
    }

    #[tokio::test]
    async fn test_add_allotment_extends_existing_session() {
        let store = InMemorySessionStore::new();

        // First allotment
        let s1 = add_allotment(&store, "aa:bb:cc:dd:ee:ff", "milliseconds", 60_000)
            .await
            .unwrap();
        let first_start = s1.start_time;

        // Advance time a tiny bit (the second call will get a new timestamp)
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Second allotment — should extend
        let s2 = add_allotment(&store, "aa:bb:cc:dd:ee:ff", "milliseconds", 30_000)
            .await
            .unwrap();

        assert_eq!(s2.allotment, 90_000);
        assert!(s2.start_time >= first_start);
        assert_eq!(s2.mac_address, "aa:bb:cc:dd:ee:ff");
    }

    #[tokio::test]
    async fn test_add_allotment_multiple_additions() {
        let store = InMemorySessionStore::new();

        add_allotment(&store, "aa:bb:cc:dd:ee:ff", "bytes", 100)
            .await
            .unwrap();
        add_allotment(&store, "aa:bb:cc:dd:ee:ff", "bytes", 200)
            .await
            .unwrap();
        let s3 = add_allotment(&store, "aa:bb:cc:dd:ee:ff", "bytes", 300)
            .await
            .unwrap();

        assert_eq!(s3.allotment, 600);
        assert_eq!(s3.metric, "bytes");

        // Verify persisted
        let stored = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(stored.allotment, 600);
    }
}
