//! Session store for the v1_compat REST API.
//!
//! Provides session state for the session_api endpoints
//! (tollgate-auth consumer). Two backends: InMemorySessionStore (default)
//! and SqliteSessionStore (`session-store-path` config), which keeps
//! sessions across restarts. rusqlite is pinned to =0.31 so it shares
//! libsqlite3-sys 0.28 with cdk-sqlite instead of dragging in a second
//! native sqlite3 build.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
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

    /// Idempotent variant of [`SessionStore::add_allotment`] keyed by the
    /// paid token: the first call for a token credits the session and
    /// durably records the token; later calls for the SAME token are
    /// no-ops returning `None`. Crash recovery re-derives an owed grant
    /// from the journal (checkstate SPENT) and must not double-credit a
    /// session that already holds the credit — the token record and the
    /// allotment add commit atomically, so exactly one side of a crash
    /// lands.
    async fn credit_for_token(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        token: &str,
        now: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;

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
    // saturating: start_time ahead of `now` (clock step back) and extreme
    // allotments must not panic the renewal path with add/sub overflows.
    let elapsed_secs = now.saturating_sub(session.start_time).max(0);
    let consumed_secs = if session.metric == "milliseconds" {
        (elapsed_secs.saturating_mul(1000))
            .min(i64::try_from(session.allotment).unwrap_or(i64::MAX))
            / 1000
    } else {
        0
    };
    session.allotment = session.allotment.saturating_add(allotment);
    session.start_time = now.saturating_sub(consumed_secs);
}

/// In-memory session store (tokio Mutex<HashMap>).
#[derive(Default)]
pub struct InMemorySessionStore {
    map: Mutex<HashMap<String, CustomerSession>>,
    credited: Mutex<std::collections::HashSet<String>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn is_time_expired(s: &CustomerSession, now_secs: i64) -> bool {
    s.metric == "milliseconds"
        && now_secs.saturating_sub(s.start_time).saturating_mul(1000) >= s.allotment as i64
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

    async fn credit_for_token(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        token: &str,
        now: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        if !self.credited.lock().await.insert(token.to_owned()) {
            return Ok(None);
        }
        // Process-local like the sessions: a restart loses both the
        // allotment and the dedupe set together, so recovery from a
        // journal entry always starts from zero.
        self.add_allotment(mac, metric, amount, now).await.map(Some)
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

/// SQLite-backed session store: one row per MAC, the session serialized as
/// JSON. Like the in-memory store, every read-modify-write runs under the
/// connection lock so overlapping payments cannot lose each other's top-ups.
pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
}

fn db_err(err: rusqlite::Error) -> SessionStoreError {
    SessionStoreError::Other(err.to_string())
}

fn json_err(err: serde_json::Error) -> SessionStoreError {
    SessionStoreError::Other(err.to_string())
}

fn load_session(
    conn: &Connection,
    mac: &str,
) -> Result<Option<CustomerSession>, SessionStoreError> {
    conn.query_row(
        "SELECT session_json FROM sessions WHERE mac = ?1",
        [mac],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(db_err)?
    .map(|json| serde_json::from_str(&json).map_err(json_err))
    .transpose()
}

fn save_session(
    conn: &Connection,
    mac: &str,
    session: &CustomerSession,
) -> Result<(), SessionStoreError> {
    let json = serde_json::to_string(session).map_err(json_err)?;
    conn.execute(
        "INSERT OR REPLACE INTO sessions (mac, session_json) VALUES (?1, ?2)",
        params![mac, json],
    )
    .map(|_| ())
    .map_err(db_err)
}

fn delete_session(conn: &Connection, mac: &str) -> Result<(), SessionStoreError> {
    conn.execute("DELETE FROM sessions WHERE mac = ?1", [mac])
        .map(|_| ())
        .map_err(db_err)
}

fn load_all_sessions(conn: &Connection) -> Result<Vec<CustomerSession>, SessionStoreError> {
    let mut stmt = conn
        .prepare("SELECT session_json FROM sessions")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err)?;
    let mut sessions = Vec::new();
    for row in rows {
        let json = row.map_err(db_err)?;
        sessions.push(serde_json::from_str(&json).map_err(json_err)?);
    }
    Ok(sessions)
}

impl SqliteSessionStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                mac TEXT PRIMARY KEY,
                session_json TEXT NOT NULL
            )",
            (),
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS credited_tokens (
                token TEXT PRIMARY KEY
            )",
            (),
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[async_trait::async_trait]
impl SessionStore for SqliteSessionStore {
    async fn get(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        load_session(&conn, mac)
    }

    async fn insert(&self, session: CustomerSession) -> Result<(), SessionStoreError> {
        let conn = self.conn.lock().await;
        save_session(&conn, &session.mac_address, &session)
    }

    async fn remove(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let removed = load_session(&conn, mac)?;
        if removed.is_some() {
            delete_session(&conn, mac)?;
        }
        Ok(removed)
    }

    async fn update(&self, mac: &str, session: CustomerSession) -> Result<(), SessionStoreError> {
        let conn = self.conn.lock().await;
        save_session(&conn, mac, &session)
    }

    async fn list_all(&self) -> Result<Vec<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        load_all_sessions(&conn)
    }

    async fn list_expired(&self, now_secs: i64) -> Result<Vec<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        Ok(load_all_sessions(&conn)?
            .into_iter()
            .filter(|s| is_time_expired(s, now_secs))
            .collect())
    }

    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError> {
        let conn = self.conn.lock().await;
        let session = match load_session(&conn, mac)? {
            Some(mut session) => {
                apply_renewal(&mut session, amount, now);
                session
            }
            None => CustomerSession {
                mac_address: mac.to_owned(),
                start_time: now,
                metric: metric.to_owned(),
                allotment: amount,
                last_external_usage: None,
            },
        };
        save_session(&conn, mac, &session)?;
        Ok(session)
    }

    async fn credit_for_token(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        token: &str,
        now: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let mut conn = self.conn.lock().await;
        // Token claim + session write in ONE transaction: a crash either
        // lands both (credit owed, entry completable) or neither (entry
        // stays journaled, recovery re-grants).
        let tx = conn.transaction().map_err(db_err)?;
        let claimed = tx
            .execute(
                "INSERT OR IGNORE INTO credited_tokens (token) VALUES (?1)",
                params![token],
            )
            .map_err(db_err)?;
        if claimed == 0 {
            return Ok(None);
        }
        let session = match load_session(&tx, mac)? {
            Some(mut session) => {
                apply_renewal(&mut session, amount, now);
                session
            }
            None => CustomerSession {
                mac_address: mac.to_owned(),
                start_time: now,
                metric: metric.to_owned(),
                allotment: amount,
                last_external_usage: None,
            },
        };
        save_session(&tx, mac, &session)?;
        tx.commit().map_err(db_err)?;
        Ok(Some(session))
    }

    async fn record_external_usage(
        &self,
        mac: &str,
        snapshot: ExternalUsageSnapshot,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        match load_session(&conn, mac)? {
            Some(mut session) => {
                session.last_external_usage = Some(snapshot);
                save_session(&conn, mac, &session)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    async fn remove_expired(
        &self,
        mac: &str,
        now_secs: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        match load_session(&conn, mac)? {
            Some(session) if is_time_expired(&session, now_secs) => {
                delete_session(&conn, mac)?;
                Ok(Some(session))
            }
            _ => Ok(None),
        }
    }

    async fn remove_exhausted_bytes(
        &self,
        mac: &str,
        usage: u64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        match load_session(&conn, mac)? {
            Some(session) if usage >= session.allotment => {
                delete_session(&conn, mac)?;
                Ok(Some(session))
            }
            _ => Ok(None),
        }
    }
}

/// Build the session store the v1 config asks for: SQLite when
/// `session_store_path` is set, in-memory otherwise. A configured path that
/// fails to open logs and degrades to in-memory so the server still serves
/// (sessions then do not survive a restart).
pub fn session_store_from_config(path: Option<&str>) -> std::sync::Arc<dyn SessionStore> {
    match path {
        Some(path) => match SqliteSessionStore::open(path) {
            Ok(store) => std::sync::Arc::new(store),
            Err(err) => {
                tracing::error!(
                    path,
                    %err,
                    "session store open failed; falling back to in-memory (sessions will not survive a restart)"
                );
                std::sync::Arc::new(InMemorySessionStore::new())
            }
        },
        None => std::sync::Arc::new(InMemorySessionStore::new()),
    }
}

/// Whether a session is retired: non-active (metering semantics of
/// [`super::session_api::session_metric_usage_at`], so a session with
/// remaining quota is NEVER a candidate) AND past the retention window.
/// Millisecond sessions age from creation plus their allotment; byte
/// sessions age from their last external usage report (no wall-clock
/// expiry of their own), and one without any report has no age anchor
/// and is kept.
fn is_retired(session: &CustomerSession, now_secs: i64, retention_secs: u64) -> bool {
    let (_, remaining) = super::session_api::session_metric_usage_at(session, now_secs);
    if remaining > 0 {
        return false;
    }
    let retention = i64::try_from(retention_secs).unwrap_or(i64::MAX);
    if session.metric == "milliseconds" {
        let allotted_secs = i64::try_from(session.allotment / 1000).unwrap_or(i64::MAX);
        now_secs
            > session
                .start_time
                .saturating_add(allotted_secs)
                .saturating_add(retention)
    } else {
        session
            .last_external_usage
            .as_ref()
            .is_some_and(|u| now_secs > u.reported_at.saturating_add(retention))
    }
}

/// One retention-janitor pass: remove every retired session. Returns the
/// number removed. `retention_secs == 0` disables retention entirely
/// (nothing is removed).
///
/// Candidates come from [`SessionStore::list_all`], but each removal goes
/// through [`SessionStore::remove_expired`] /
/// [`SessionStore::remove_exhausted_bytes`], which re-check exhaustion
/// under the store lock — a renewal racing between the listing and the
/// removal keeps its session.
pub(crate) async fn sweep_retired_sessions(
    store: &dyn SessionStore,
    now_secs: i64,
    retention_secs: u64,
) -> Result<usize, SessionStoreError> {
    if retention_secs == 0 {
        return Ok(0);
    }
    let mut removed = 0;
    for session in store.list_all().await? {
        if !is_retired(&session, now_secs, retention_secs) {
            continue;
        }
        let gone = if session.metric == "milliseconds" {
            store.remove_expired(&session.mac_address, now_secs).await?
        } else {
            let (consumed, _) = super::session_api::session_metric_usage_at(&session, now_secs);
            let consumed = u64::try_from(consumed).unwrap_or(u64::MAX);
            store
                .remove_exhausted_bytes(&session.mac_address, consumed)
                .await?
        };
        if gone.is_some() {
            removed += 1;
            tracing::debug!(
                mac = %session.mac_address,
                metric = %session.metric,
                "[session-janitor] removed retired session"
            );
        }
    }
    Ok(removed)
}

/// Spawn the background session retention janitor (the spawn pattern of
/// [`super::payout::spawn_payout_task`]): sweep once at boot (a store
/// that accumulated retired sessions while the janitor did not exist is
/// cleaned on the first start), then hourly. Never crashes; store errors
/// are logged and retried on the next tick. `retention_secs == 0` does
/// not spawn at all.
pub fn spawn_session_janitor(
    store: std::sync::Arc<dyn SessionStore>,
    retention_secs: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    if retention_secs == 0 {
        tracing::info!(
            "[session-janitor] retention disabled (session_retention_secs=0); sessions are kept forever"
        );
        return None;
    }
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match sweep_retired_sessions(store.as_ref(), now_secs(), retention_secs).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("[session-janitor] removed {n} retired sessions"),
                Err(e) => tracing::warn!("[session-janitor] sweep failed: {e}"),
            }
        }
    }))
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

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tg-session-store-{}-{label}.sqlite",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn sqlite_credit_for_token_is_idempotent_and_persists() {
        let path = temp_db_path("credit-token");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSessionStore::open(&path).unwrap();
        let mac = "02:b3:a0:00:00:01";

        let first = store
            .credit_for_token(mac, "milliseconds", 240_000, "cashuBt1", 1_000)
            .await
            .unwrap()
            .expect("first credit lands");
        assert_eq!(first.allotment, 240_000);

        let dup = store
            .credit_for_token(mac, "milliseconds", 240_000, "cashuBt1", 1_100)
            .await
            .unwrap();
        assert!(dup.is_none(), "same token must not credit twice");
        assert_eq!(
            store.get(mac).await.unwrap().unwrap().allotment,
            240_000,
            "duplicate credit must leave the allotment untouched"
        );

        let second = store
            .credit_for_token(mac, "milliseconds", 60_000, "cashuBt2", 1_200)
            .await
            .unwrap()
            .expect("a different token is a legitimate top-up");
        assert_eq!(second.allotment, 300_000);

        // The dedupe record is durable: a restart (reopen) still skips a
        // re-delivery of an already-credited token.
        drop(store);
        let reopened = SqliteSessionStore::open(&path).unwrap();
        let dup = reopened
            .credit_for_token(mac, "milliseconds", 240_000, "cashuBt1", 2_000)
            .await
            .unwrap();
        assert!(dup.is_none(), "credited-token record must survive reopen");
        assert_eq!(reopened.get(mac).await.unwrap().unwrap().allotment, 300_000);

        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn in_memory_credit_for_token_skips_duplicates() {
        let store = InMemorySessionStore::new();
        let mac = "02:b3:a0:00:00:02";
        store
            .credit_for_token(mac, "milliseconds", 60_000, "cashuBt1", 1_000)
            .await
            .unwrap()
            .expect("first credit lands");
        let dup = store
            .credit_for_token(mac, "milliseconds", 60_000, "cashuBt1", 1_100)
            .await
            .unwrap();
        assert!(dup.is_none());
        assert_eq!(store.get(mac).await.unwrap().unwrap().allotment, 60_000);
    }

    #[tokio::test]
    async fn sqlite_add_allotment_creates_and_tops_up() {
        let path = temp_db_path("allotment");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSessionStore::open(&path).unwrap();

        let s1 = store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 60_000, 1000)
            .await
            .unwrap();
        assert_eq!(s1.allotment, 60_000);

        let s2 = store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 30_000, 1005)
            .await
            .unwrap();
        // Same renewal semantics as the in-memory store: 60_000 + 30_000
        // total allotment, start_time adjusted to preserve the 5s consumed.
        assert_eq!(s2.allotment, 90_000);
        assert_eq!(s2.start_time, 1000);

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn sqlite_get_remove_roundtrip() {
        let path = temp_db_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSessionStore::open(&path).unwrap();
        let mac = "aa:bb:cc:dd:ee:ff";

        store
            .add_allotment(mac, "bytes", 1_024, 1_000)
            .await
            .unwrap();
        let got = store.get(mac).await.unwrap().expect("session present");
        assert_eq!(got.mac_address, mac);
        assert_eq!(got.allotment, 1_024);

        let removed = store.remove(mac).await.unwrap().expect("session removed");
        assert_eq!(removed.allotment, 1_024);
        assert!(store.get(mac).await.unwrap().is_none());
        assert!(store.remove(mac).await.unwrap().is_none());

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn sqlite_sessions_survive_reopen() {
        let path = temp_db_path("reopen");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSessionStore::open(&path).unwrap();
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
        store
            .record_external_usage("aa:bb:cc:dd:ee:ff", snapshot.clone())
            .await
            .unwrap();
        drop(store);

        let reopened = SqliteSessionStore::open(&path).unwrap();
        let session = reopened
            .get("aa:bb:cc:dd:ee:ff")
            .await
            .unwrap()
            .expect("session survives reopen");
        assert_eq!(session.allotment, 5_000);
        assert_eq!(session.last_external_usage, Some(snapshot));

        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn renewal_and_expiry_survive_extreme_values() {
        // Regression (panic audit 2026-11-16): apply_renewal and
        // is_time_expired panicked with add/sub overflows on start_time
        // ahead of `now` (clock step) and extreme allotments.
        let mut session = CustomerSession {
            mac_address: "aa:bb:cc:dd:ee:ff".to_owned(),
            start_time: i64::MAX,
            metric: "milliseconds".to_owned(),
            allotment: u64::MAX,
            last_external_usage: None,
        };
        apply_renewal(&mut session, u64::MAX, i64::MIN);
        assert_eq!(session.start_time, i64::MIN, "consumed_secs clamps to 0");
        assert_eq!(session.allotment, u64::MAX, "allotment saturates");

        let renewed = CustomerSession {
            mac_address: String::new(),
            start_time: i64::MIN,
            metric: "milliseconds".to_owned(),
            allotment: i64::MAX as u64,
            last_external_usage: None,
        };
        assert!(!is_time_expired(&renewed, i64::MIN));
        assert!(is_time_expired(
            &CustomerSession {
                mac_address: String::new(),
                start_time: i64::MIN,
                metric: "milliseconds".to_owned(),
                allotment: 1,
                last_external_usage: None,
            },
            i64::MAX
        ));
    }

    const RETENTION: u64 = 604_800;

    async fn janitor_fixture() -> InMemorySessionStore {
        let store = InMemorySessionStore::new();
        // active ms session (11+ days of quota left)
        store
            .add_allotment("02:00:00:00:00:01", "milliseconds", 1_000_000_000, 0)
            .await
            .unwrap();
        // active bytes session with an ANCIENT usage report: remaining
        // quota must protect it even past the retention window
        store
            .add_allotment("02:00:00:00:00:02", "bytes", 5_000, 0)
            .await
            .unwrap();
        store
            .record_external_usage(
                "02:00:00:00:00:02",
                ExternalUsageSnapshot {
                    input_octets: 100,
                    output_octets: 200,
                    session_time: 0,
                    reported_at: 1_000,
                },
            )
            .await
            .unwrap();
        // suspended ms session, expired far past the retention window
        store
            .add_allotment("02:00:00:00:00:03", "milliseconds", 60_000, 0)
            .await
            .unwrap();
        // suspended ms session, expired 5s before the sweep at t=700_000
        // (allotment covers 700_000s from t=0)
        store
            .add_allotment("02:00:00:00:00:04", "milliseconds", 700_000_000, 0)
            .await
            .unwrap();
        // exhausted bytes session, last report far past the window
        store
            .add_allotment("02:00:00:00:00:05", "bytes", 1_000, 0)
            .await
            .unwrap();
        store
            .record_external_usage(
                "02:00:00:00:00:05",
                ExternalUsageSnapshot {
                    input_octets: 1_200,
                    output_octets: 800,
                    session_time: 0,
                    reported_at: 1_000,
                },
            )
            .await
            .unwrap();
        // exhausted bytes session, last report 5s ago (inside the window)
        store
            .add_allotment("02:00:00:00:00:06", "bytes", 1_000, 0)
            .await
            .unwrap();
        store
            .record_external_usage(
                "02:00:00:00:00:06",
                ExternalUsageSnapshot {
                    input_octets: 1_200,
                    output_octets: 800,
                    session_time: 0,
                    reported_at: 700_000,
                },
            )
            .await
            .unwrap();
        store
    }

    #[tokio::test]
    async fn janitor_never_evicts_sessions_with_remaining_quota() {
        let store = janitor_fixture().await;
        // 700_000: session :03 is 60s-expired + past the 7-day window,
        // :05's report (1_000 + 604_800 < 700_000) is past it too — but
        // :01/:02 still hold quota and must survive.
        let now = 700_000;
        sweep_retired_sessions(&store, now, RETENTION)
            .await
            .unwrap();
        assert!(store.get("02:00:00:00:00:01").await.unwrap().is_some());
        assert!(
            store.get("02:00:00:00:00:02").await.unwrap().is_some(),
            "active bytes session with an ancient usage report must survive"
        );
    }

    #[tokio::test]
    async fn janitor_evicts_suspended_sessions_past_retention() {
        let store = janitor_fixture().await;
        let removed = sweep_retired_sessions(&store, 700_000, RETENTION)
            .await
            .unwrap();
        assert_eq!(removed, 2);
        assert!(store.get("02:00:00:00:00:03").await.unwrap().is_none());
        assert!(store.get("02:00:00:00:00:05").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn janitor_keeps_suspended_sessions_within_retention() {
        let store = janitor_fixture().await;
        // At now=700_005: :03/:05 are past the window (removed), but :04
        // expired only 5s ago and :06 last reported 5s ago — both still
        // inside the 7-day window and must be kept.
        let removed = sweep_retired_sessions(&store, 700_005, RETENTION)
            .await
            .unwrap();
        assert_eq!(removed, 2);
        assert!(store.get("02:00:00:00:00:04").await.unwrap().is_some());
        assert!(store.get("02:00:00:00:00:06").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn janitor_retention_zero_disables_sweep() {
        let store = janitor_fixture().await;
        let removed = sweep_retired_sessions(&store, 10_000_000_000, 0)
            .await
            .unwrap();
        assert_eq!(removed, 0);
        // everything survives, even long-retired sessions
        for mac in [
            "02:00:00:00:00:01",
            "02:00:00:00:00:02",
            "02:00:00:00:00:03",
            "02:00:00:00:00:04",
            "02:00:00:00:00:05",
            "02:00:00:00:00:06",
        ] {
            assert!(store.get(mac).await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn janitor_spares_session_renewed_during_sweep() {
        // The conditional removes re-check exhaustion under the lock: a
        // top-up landing between list_all and the removal keeps the
        // session (remove_expired on a renewed record is a no-op).
        let store = InMemorySessionStore::new();
        store
            .add_allotment("02:00:00:00:00:07", "milliseconds", 60_000, 0)
            .await
            .unwrap();
        let now = 700_000;
        assert!(is_retired(
            &store.get("02:00:00:00:00:07").await.unwrap().unwrap(),
            now,
            RETENTION
        ));
        store
            .add_allotment("02:00:00:00:00:07", "milliseconds", 1_000_000_000, now)
            .await
            .unwrap();
        let removed = sweep_retired_sessions(&store, now, RETENTION)
            .await
            .unwrap();
        assert_eq!(removed, 0);
        assert!(store.get("02:00:00:00:00:07").await.unwrap().is_some());
    }
}
