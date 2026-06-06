#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::{SessionStore, Valve};

pub fn spawn_data_monitor(
    sessions: Arc<dyn SessionStore>,
    valve: Arc<dyn Valve + Send + Sync>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;

            let all = match sessions.list_all().await {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!("data_monitor: failed to list sessions: {e}");
                    continue;
                }
            };

            let bytes_sessions: Vec<_> = all
                .into_iter()
                .filter(|s| s.metric != "milliseconds")
                .collect();

            for session in bytes_sessions {
                let usage = match valve.get_client_usage_since_baseline(&session.mac_address).await {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::debug!(
                            "data_monitor: could not read usage for mac={}: {e}",
                            session.mac_address,
                        );
                        continue;
                    }
                };

                if usage >= session.allotment {
                    tracing::info!(
                        "data_monitor: quota exceeded mac={} usage={} allotment={}",
                        session.mac_address,
                        usage,
                        session.allotment,
                    );

                    if let Err(e) = sessions.remove(&session.mac_address).await {
                        tracing::warn!(
                            "data_monitor: failed to remove session mac={}: {e}",
                            session.mac_address,
                        );
                        continue;
                    }

                    if let Err(e) = valve.close_gate(&session.mac_address).await {
                        tracing::warn!(
                            "data_monitor: failed to close valve for mac={}: {e}",
                            session.mac_address,
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
    use crate::v1::server::{CustomerSession, InMemorySessionStore};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::atomic::AtomicBool;

    struct MockBytesValve {
        usage: AtomicU64,
        closed: AtomicBool,
    }

    impl MockBytesValve {
        fn new(initial_usage: u64) -> Self {
            Self {
                usage: AtomicU64::new(initial_usage),
                closed: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl Valve for MockBytesValve {
        async fn open_gate(&self, _mac_address: &str) -> Result<(), super::super::ValveError> {
            Ok(())
        }

        async fn close_gate(&self, _mac_address: &str) -> Result<(), super::super::ValveError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn get_client_usage_since_baseline(
            &self,
            _mac_address: &str,
        ) -> Result<u64, super::super::ValveError> {
            Ok(self.usage.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn data_monitor_closes_gate_when_bytes_exceeded() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(MockBytesValve::new(2000));

        let session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: 0,
            metric: "bytes".to_owned(),
            allotment: 1000,
        };
        store.insert(session).await.unwrap();

        let handle = spawn_data_monitor(
            store.clone(),
            valve.clone(),
            Duration::from_millis(50),
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_none(),
            "bytes session should have been removed when usage exceeded allotment",
        );
        assert!(
            valve.closed.load(Ordering::SeqCst),
            "valve should have been closed",
        );
    }

    #[tokio::test]
    async fn data_monitor_skips_milliseconds_sessions() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(MockBytesValve::new(99999));

        let session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: 0,
            metric: "milliseconds".to_owned(),
            allotment: 1000,
        };
        store.insert(session.clone()).await.unwrap();

        let handle = spawn_data_monitor(
            store.clone(),
            valve.clone(),
            Duration::from_millis(50),
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_some(),
            "milliseconds session should be left alone by data monitor",
        );
    }

    #[tokio::test]
    async fn data_monitor_keeps_session_under_quota() {
        let store = Arc::new(InMemorySessionStore::new());
        let valve = Arc::new(MockBytesValve::new(500));

        let session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: 0,
            metric: "bytes".to_owned(),
            allotment: 1000,
        };
        store.insert(session).await.unwrap();

        let handle = spawn_data_monitor(
            store.clone(),
            valve.clone(),
            Duration::from_millis(50),
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_some(),
            "session under quota should remain active",
        );
        assert!(
            !valve.closed.load(Ordering::SeqCst),
            "valve should not have been closed",
        );
    }
}
