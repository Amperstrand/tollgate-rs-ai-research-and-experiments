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
/// [`SessionStore::list_expired`] for wall-clock-expired sessions, then
/// evaluate byte-metered sessions against [`Valve::get_client_usage_since_baseline`]
/// (the store alone cannot judge byte consumption). Expired sessions are
/// removed from the store and their valve closed. Errors are logged at
/// `warn` level but never crash the loop.
///
/// Returns a [`JoinHandle`] so the caller can abort the task on shutdown.
pub fn spawn_janitor(
    sessions: Arc<dyn SessionStore>,
    valve: Arc<dyn Valve + Send + Sync>,
    interval: Duration,
) -> JoinHandle<()> {
    async fn evict(
        sessions: &Arc<dyn SessionStore>,
        valve: &Arc<dyn Valve + Send + Sync>,
        session: &super::CustomerSession,
        reason: &str,
    ) {
        match sessions.remove(&session.mac_address).await {
            Ok(_) => {
                tracing::info!(
                    "janitor: evicted session mac={} allotment={} ({reason})",
                    session.mac_address,
                    session.allotment,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "janitor: failed to remove session mac={}: {e}",
                    session.mac_address,
                );
                return;
            }
        }

        if let Err(e) = valve.close_gate(&session.mac_address) {
            tracing::warn!(
                "janitor: failed to close valve for mac={}: {e}",
                session.mac_address,
            );
        }
    }

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
                evict(&sessions, &valve, &session, "time quota exhausted").await;
            }

            let all = match sessions.list_all().await {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!("janitor: failed to list sessions: {e}");
                    continue;
                }
            };

            for session in all
                .iter()
                .filter(|s| s.metric != "milliseconds")
            {
                let usage = valve
                    .get_client_usage_since_baseline(&session.mac_address)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "janitor: failed to read byte usage for mac={}: {e}",
                            session.mac_address,
                        );
                        0
                    });

                if usage >= session.allotment {
                    evict(&sessions, &valve, session, "byte quota exhausted").await;
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

#[cfg(test)]
mod byte_tests {
    use super::*;
    use crate::v1::server::{CustomerSession, InMemorySessionStore, Valve, ValveError};
    use std::sync::Mutex;

    struct MeteredValve {
        usage: u64,
        closed: Mutex<Vec<String>>,
    }

    impl MeteredValve {
        fn new(usage: u64) -> Self {
            Self {
                usage,
                closed: Mutex::new(Vec::new()),
            }
        }
    }

    impl Valve for MeteredValve {
        fn open_gate(&self, _mac_address: &str) -> Result<(), ValveError> {
            Ok(())
        }

        fn close_gate(&self, mac_address: &str) -> Result<(), ValveError> {
            self.closed.lock().unwrap().push(mac_address.to_owned());
            Ok(())
        }

        fn get_client_usage_since_baseline(
            &self,
            _mac_address: &str,
        ) -> Result<u64, ValveError> {
            Ok(self.usage)
        }
    }

    fn byte_session(mac: &str, allotment: u64) -> CustomerSession {
        CustomerSession {
            mac_address: mac.to_owned(),
            start_time: 0,
            metric: "bytes".to_owned(),
            allotment,
        }
    }

    #[tokio::test]
    async fn janitor_evicts_byte_session_past_quota() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(MeteredValve::new(6_000));

        store.insert(byte_session("aa:bb:cc:dd:ee:ff", 5_000)).await.unwrap();

        let handle = spawn_janitor(store.clone(), valve.clone(), Duration::from_millis(50));
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_none(),
            "byte session past its quota must be evicted"
        );
        assert_eq!(
            *valve.closed.lock().unwrap(),
            vec!["aa:bb:cc:dd:ee:ff".to_owned()],
            "the valve must be closed for the evicted byte session"
        );
    }

    #[tokio::test]
    async fn janitor_keeps_byte_session_under_quota() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(MeteredValve::new(1_000));

        let active = byte_session("aa:bb:cc:dd:ee:ff", 5_000);
        store.insert(active.clone()).await.unwrap();

        let handle = spawn_janitor(store.clone(), valve, Duration::from_millis(50));
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert_eq!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap(),
            Some(active),
            "byte session under its quota must stay active"
        );
    }
}
