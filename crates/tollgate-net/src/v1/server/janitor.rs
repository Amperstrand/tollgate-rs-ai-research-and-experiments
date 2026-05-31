#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::task::JoinHandle;

use super::{SessionStore, Valve};

/// Spawns a background task that periodically evicts expired sessions.
///
/// The janitor loops forever: sleep for `interval`, query
/// [`SessionStore::list_expired`], remove each expired session from the
/// store, and close its valve.  Errors are logged at `warn` level but
/// never crash the loop.
///
/// Returns a [`JoinHandle`] so the caller can abort the task on shutdown.
pub fn spawn_janitor(
    sessions: Arc<dyn SessionStore>,
    valve: Arc<dyn Valve + Send + Sync>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;

            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            let expired = match sessions.list_expired(now_secs).await {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!("janitor: failed to list expired sessions: {e}");
                    continue;
                }
            };

            for session in expired {
                match sessions.remove(&session.mac_address).await {
                    Ok(_) => {
                        tracing::info!(
                            "janitor: evicted session mac={} allotment={}",
                            session.mac_address,
                            session.allotment,
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "janitor: failed to remove session mac={}: {e}",
                            session.mac_address,
                        );
                        continue;
                    }
                }

                if let Err(e) = valve.close_gate(&session.mac_address).await {
                    tracing::warn!(
                        "janitor: failed to close valve for mac={}: {e}",
                        session.mac_address,
                    );
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::server::{CustomerSession, InMemorySessionStore, StubValve};

    #[tokio::test]
    async fn janitor_evicts_expired_sessions() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(StubValve);

        // start_time=100 → (now_secs - 100) * 1000 >> allotment=1000
        let expired = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: 100,
            metric: "milliseconds".to_owned(),
            allotment: 1000,
        };
        store.insert(expired).await.unwrap();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let active = CustomerSession {
            mac_address: "11:22:33:44:55:66".to_owned(),
            start_time: now,
            metric: "milliseconds".to_owned(),
            allotment: 3_600_000,
        };
        store.insert(active.clone()).await.unwrap();

        let handle = spawn_janitor(store.clone(), valve, Duration::from_millis(50));

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_none(),
            "expired session should have been removed"
        );
        assert_eq!(
            store.get("11:22:33:44:55:66").await.unwrap(),
            Some(active),
            "active session should still be present"
        );
    }
}
