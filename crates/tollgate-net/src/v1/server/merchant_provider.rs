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
/// If a session for `mac` already exists, the top-up adds to its remaining
/// balance: consumed time is not restored (see [`apply_renewal`]). A new
/// session is created with the given `metric` and `allotment`.
/// Extend a session's allotment without restoring consumed time.
///
/// `now`/`start_time` are unix seconds while `allotment` is in the
/// session's metric units (milliseconds for time metrics): the clamp
/// converts elapsed seconds to the allotment's unit so an expired
/// session's renewal grants the full fresh purchase. Non-time metrics
/// (bytes) are not consumed by wall-clock, so their start_time resets.
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

pub async fn add_allotment(
    sessions: &dyn SessionStore,
    mac: &str,
    metric: &str,
    allotment: u64,
) -> Result<CustomerSession, String> {
    #[allow(clippy::cast_possible_wrap)]
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let existing = sessions.get(mac).await.map_err(|e| e.to_string())?;

    let session = if let Some(mut s) = existing {
        apply_renewal(&mut s, allotment, now);
        let updated = s.clone();
        sessions.update(mac, s).await.map_err(|e| e.to_string())?;
        updated
    } else {
        let s = CustomerSession {
            mac_address: mac.to_owned(),
            start_time: now,
            metric: metric.to_owned(),
            allotment,
            last_external_usage: None,
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

    fn renewed_session(start_time: i64, allotment: u64) -> CustomerSession {
        CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time,
            metric: "milliseconds".to_owned(),
            allotment,
            last_external_usage: None,
        }
    }

    fn remaining(s: &CustomerSession, now_secs: i64) -> i64 {
        i64::try_from(s.allotment).unwrap_or(i64::MAX) - (now_secs - s.start_time) * 1000
    }

    #[test]
    fn renewal_adds_to_remaining_balance_not_original_purchase() {
        // 60s bought, 50s consumed, 60s more purchased -> 70s remain.
        let mut s = renewed_session(0, 60_000);
        apply_renewal(&mut s, 60_000, 50);
        assert_eq!(remaining(&s, 50), 70_000);
    }

    #[test]
    fn renewal_of_expired_session_grants_fresh_allotment_only() {
        let mut s = renewed_session(0, 60_000);
        apply_renewal(&mut s, 60_000, 300);
        assert_eq!(remaining(&s, 300), 60_000);
    }

    #[test]
    fn renewal_at_exact_expiry_carries_no_deficit() {
        let mut s = renewed_session(0, 60_000);
        apply_renewal(&mut s, 30_000, 60);
        assert_eq!(remaining(&s, 60), 30_000);
    }

    #[test]
    fn expired_ms_session_renewal_grants_full_fresh_purchase() {
        // 60s session expired 2s ago (janitor not yet run); a new 60s
        // purchase must grant a full fresh 60s, not 58s.
        let mut s = renewed_session(0, 60_000);
        apply_renewal(&mut s, 60_000, 62);
        assert_eq!(remaining(&s, 62), 60_000);
    }

    #[test]
    fn repeated_renewals_do_not_stack_time() {
        // 3x60s paid, 120s consumed by t=120s -> exactly the third grant
        // remains; no consumed time is ever restored.
        let mut s = renewed_session(0, 60_000);
        apply_renewal(&mut s, 60_000, 60);
        apply_renewal(&mut s, 60_000, 120);
        assert_eq!(remaining(&s, 120), 60_000);
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
