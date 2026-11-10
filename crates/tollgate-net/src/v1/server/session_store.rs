#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{CustomerSession, ExternalUsageSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn get(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError>;
    async fn insert(&self, session: CustomerSession) -> Result<(), SessionStoreError>;
    async fn remove(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError>;
    async fn update(&self, mac: &str, session: CustomerSession) -> Result<(), SessionStoreError>;
    async fn list_all(&self) -> Result<Vec<CustomerSession>, SessionStoreError>;
    /// Find sessions whose `allotment` (interpreted as milliseconds when
    /// `metric == "milliseconds"`) has been fully consumed by time elapsed
    /// since `start_time`.
    async fn list_expired(&self, now_secs: i64) -> Result<Vec<CustomerSession>, SessionStoreError>;
    /// Atomically add `amount` to the MAC's allotment (renewal semantics
    /// per `merchant_provider::apply_renewal`), creating the session when
    /// absent. The read-modify-write happens under the store's own lock so
    /// overlapping payments cannot lose each other's tokens-already-redeemed
    /// top-ups.
    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError>;
    /// Atomically record an external-usage snapshot, touching ONLY the
    /// snapshot — a concurrent renewal's allotment write cannot be clobbered
    /// by a report's stale session clone.
    async fn record_external_usage(
        &self,
        mac: &str,
        snapshot: ExternalUsageSnapshot,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;
    /// Evict the session ONLY if the CURRENT record (under the store lock)
    /// is still wall-clock expired for `now_secs` — a renewal that landed
    /// after the caller's snapshot must survive the eviction.
    async fn remove_expired(
        &self,
        mac: &str,
        now_secs: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;
    /// Evict the session ONLY if the CURRENT record's allotment is still
    /// exhausted by `usage` bytes — a top-up that landed after the caller's
    /// usage snapshot must survive the eviction.
    async fn remove_exhausted_bytes(
        &self,
        mac: &str,
        usage: u64,
    ) -> Result<Option<CustomerSession>, SessionStoreError>;
}

#[derive(Default)]
pub struct InMemorySessionStore {
    map: Mutex<HashMap<String, CustomerSession>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
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
        let expired: Vec<CustomerSession> = map
            .values()
            .filter(|s| {
                if s.metric == "milliseconds" {
                    let elapsed_ms = (now_secs - s.start_time) * 1000;
                    elapsed_ms >= s.allotment as i64
                } else {
                    false
                }
            })
            .cloned()
            .collect();
        Ok(expired)
    }

    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError> {
        let mut map = self.map.lock().await;
        if let Some(session) = map.get_mut(mac) {
            super::merchant_provider::apply_renewal(session, amount, now);
            return Ok(session.clone());
        }

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
        let still_expired = map.get(mac).is_some_and(|s| {
            let elapsed_ms = (now_secs - s.start_time) * 1000;
            elapsed_ms >= s.allotment as i64
        });
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

pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}

impl SqliteSessionStore {
    const CREATE_TABLE_SQL: &str = "\
        CREATE TABLE IF NOT EXISTS sessions (\
            mac_address TEXT PRIMARY KEY, \
            start_time  INTEGER NOT NULL, \
            metric      TEXT    NOT NULL, \
            allotment   INTEGER NOT NULL, \
            last_input_octets  INTEGER, \
            last_output_octets INTEGER, \
            last_session_time  INTEGER, \
            last_reported_at   INTEGER\
        )";

    /// Add columns introduced after the first release (external-usage
    /// snapshots). `CREATE TABLE IF NOT EXISTS` cannot evolve an existing
    /// database, so missing columns are ALTERed in.
    fn migrate(conn: &rusqlite::Connection) -> Result<(), SessionStoreError> {
        let existing: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('sessions')")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for (column, decl) in [
            ("last_input_octets", "INTEGER"),
            ("last_output_octets", "INTEGER"),
            ("last_session_time", "INTEGER"),
            ("last_reported_at", "INTEGER"),
        ] {
            if !existing.iter().any(|c| c == column) {
                conn.execute(
                    &format!("ALTER TABLE sessions ADD COLUMN {column} {decl}"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    fn init_tables(conn: &rusqlite::Connection) -> Result<(), SessionStoreError> {
        conn.execute(Self::CREATE_TABLE_SQL, [])?;
        Self::migrate(conn)
    }

    /// Open (or create) a SQLite database at `path`.
    pub fn open(path: &str) -> Result<Self, SessionStoreError> {
        let conn = rusqlite::Connection::open(path)?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory() -> Result<Self, SessionStoreError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<CustomerSession> {
        let input_octets: Option<i64> = row.get(4)?;
        let output_octets: Option<i64> = row.get(5)?;
        let session_time: Option<i64> = row.get(6)?;
        let reported_at: Option<i64> = row.get(7)?;
        let last_external_usage = match (input_octets, output_octets, session_time, reported_at) {
            (Some(i), Some(o), Some(t), Some(r)) => Some(ExternalUsageSnapshot {
                input_octets: i.unsigned_abs(),
                output_octets: o.unsigned_abs(),
                session_time: t.unsigned_abs(),
                reported_at: r,
            }),
            _ => None,
        };
        Ok(CustomerSession {
            mac_address: row.get(0)?,
            start_time: row.get(1)?,
            metric: row.get(2)?,
            allotment: row.get(3)?,
            last_external_usage,
        })
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn get(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT mac_address, start_time, metric, allotment, last_input_octets, last_output_octets, last_session_time, last_reported_at FROM sessions WHERE mac_address = ?1")?;
        let session = stmt
            .query_row(rusqlite::params![mac], Self::row_to_session)
            .ok();
        Ok(session)
    }

    async fn insert(&self, session: CustomerSession) -> Result<(), SessionStoreError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sessions (mac_address, start_time, metric, allotment, \
             last_input_octets, last_output_octets, last_session_time, last_reported_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session.mac_address,
                session.start_time,
                session.metric,
                session.allotment,
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.input_octets as i64),
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.output_octets as i64),
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.session_time as i64),
                session.last_external_usage.as_ref().map(|u| u.reported_at),
            ],
        )?;
        Ok(())
    }

    async fn remove(&self, mac: &str) -> Result<Option<CustomerSession>, SessionStoreError> {
        let existing = self.get(mac).await?;
        if existing.is_some() {
            let conn = self.conn.lock().await;
            conn.execute(
                "DELETE FROM sessions WHERE mac_address = ?1",
                rusqlite::params![mac],
            )?;
        }
        Ok(existing)
    }

    async fn update(&self, mac: &str, session: CustomerSession) -> Result<(), SessionStoreError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sessions SET start_time = ?1, metric = ?2, allotment = ?3, \
             last_input_octets = ?5, last_output_octets = ?6, last_session_time = ?7, \
             last_reported_at = ?8 WHERE mac_address = ?4",
            rusqlite::params![
                session.start_time,
                session.metric,
                session.allotment,
                mac,
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.input_octets as i64),
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.output_octets as i64),
                session
                    .last_external_usage
                    .as_ref()
                    .map(|u| u.session_time as i64),
                session.last_external_usage.as_ref().map(|u| u.reported_at),
            ],
        )?;
        Ok(())
    }

    async fn list_all(&self) -> Result<Vec<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT mac_address, start_time, metric, allotment, last_input_octets, last_output_octets, last_session_time, last_reported_at FROM sessions")?;
        let sessions = stmt
            .query_map([], Self::row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    async fn list_expired(&self, now_secs: i64) -> Result<Vec<CustomerSession>, SessionStoreError> {
        // Elapsed in ms = (now_secs - start_time) * 1000
        // Expired when: elapsed_ms >= allotment
        // ⟹  (now_secs - start_time) * 1000 >= allotment
        // ⟹  now_secs - start_time >= allotment / 1000
        // We keep the multiplication to avoid integer-division edge-cases.
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT mac_address, start_time, metric, allotment, last_input_octets, last_output_octets, last_session_time, last_reported_at FROM sessions \
             WHERE metric = 'milliseconds' AND (?1 - start_time) * 1000 >= allotment",
        )?;
        let sessions = stmt
            .query_map(rusqlite::params![now_secs], Self::row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    async fn add_allotment(
        &self,
        mac: &str,
        metric: &str,
        amount: u64,
        now: i64,
    ) -> Result<CustomerSession, SessionStoreError> {
        let conn = self.conn.lock().await;
        let existing = conn
            .query_row(
                "SELECT mac_address, start_time, metric, allotment, last_input_octets, \
                 last_output_octets, last_session_time, last_reported_at FROM sessions \
                 WHERE mac_address = ?1",
                rusqlite::params![mac],
                Self::row_to_session,
            )
            .ok();

        let session = if let Some(mut s) = existing {
            super::merchant_provider::apply_renewal(&mut s, amount, now);
            // Renewal touches only start_time/allotment — the snapshot
            // columns must survive this write.
            conn.execute(
                "UPDATE sessions SET start_time = ?1, allotment = ?2 WHERE mac_address = ?3",
                rusqlite::params![s.start_time, s.allotment, mac],
            )?;
            s
        } else {
            let s = CustomerSession {
                mac_address: mac.to_owned(),
                start_time: now,
                metric: metric.to_owned(),
                allotment: amount,
                last_external_usage: None,
            };
            conn.execute(
                "INSERT INTO sessions (mac_address, start_time, metric, allotment) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![mac, s.start_time, s.metric, s.allotment],
            )?;
            s
        };
        Ok(session)
    }

    async fn remove_expired(
        &self,
        mac: &str,
        now_secs: i64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let current = conn
            .query_row(
                "SELECT mac_address, start_time, metric, allotment, last_input_octets, \
                 last_output_octets, last_session_time, last_reported_at FROM sessions \
                 WHERE mac_address = ?1",
                rusqlite::params![mac],
                Self::row_to_session,
            )
            .ok();
        let still_expired = current.as_ref().is_some_and(|s| {
            let elapsed_ms = (now_secs - s.start_time) * 1000;
            elapsed_ms >= s.allotment as i64
        });
        if !still_expired {
            return Ok(None);
        }
        conn.execute(
            "DELETE FROM sessions WHERE mac_address = ?1",
            rusqlite::params![mac],
        )?;
        Ok(current)
    }

    async fn remove_exhausted_bytes(
        &self,
        mac: &str,
        usage: u64,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let current = conn
            .query_row(
                "SELECT mac_address, start_time, metric, allotment, last_input_octets, \
                 last_output_octets, last_session_time, last_reported_at FROM sessions \
                 WHERE mac_address = ?1",
                rusqlite::params![mac],
                Self::row_to_session,
            )
            .ok();
        let still_exhausted = current.as_ref().is_some_and(|s| usage >= s.allotment);
        if !still_exhausted {
            return Ok(None);
        }
        conn.execute(
            "DELETE FROM sessions WHERE mac_address = ?1",
            rusqlite::params![mac],
        )?;
        Ok(current)
    }

    async fn record_external_usage(
        &self,
        mac: &str,
        snapshot: ExternalUsageSnapshot,
    ) -> Result<Option<CustomerSession>, SessionStoreError> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE sessions SET last_input_octets = ?1, last_output_octets = ?2, \
             last_session_time = ?3, last_reported_at = ?4 WHERE mac_address = ?5",
            rusqlite::params![
                snapshot.input_octets as i64,
                snapshot.output_octets as i64,
                snapshot.session_time as i64,
                snapshot.reported_at,
                mac,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let session = conn
            .query_row(
                "SELECT mac_address, start_time, metric, allotment, last_input_octets, \
                 last_output_octets, last_session_time, last_reported_at FROM sessions \
                 WHERE mac_address = ?1",
                rusqlite::params![mac],
                Self::row_to_session,
            )
            .ok();
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn make_session(
        mac: &str,
        start: i64,
        metric: &str,
        allotment: u64,
    ) -> CustomerSession {
        CustomerSession {
            mac_address: mac.to_owned(),
            start_time: start,
            metric: metric.to_owned(),
            allotment,
            last_external_usage: None,
        }
    }

    #[tokio::test]
    async fn sqlite_create_table_on_open() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        let all = store.list_all().await.unwrap();
        assert_eq!(all, [] as [CustomerSession; 0]);
    }

    #[tokio::test]
    async fn sqlite_insert_and_get() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        let session = make_session("aa:bb:cc:dd:ee:ff", 1000, "milliseconds", 60_000);
        store.insert(session.clone()).await.unwrap();

        let got = store.get("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(got, Some(session));

        let missing = store.get("00:00:00:00:00:00").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn sqlite_remove() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        let session = make_session("aa:bb:cc:dd:ee:ff", 1000, "milliseconds", 60_000);
        store.insert(session.clone()).await.unwrap();

        let removed = store.remove("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(removed, Some(session));

        let gone = store.get("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(gone, None);

        let remove_again = store.remove("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(remove_again, None);
    }

    #[tokio::test]
    async fn sqlite_update() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        let session = make_session("aa:bb:cc:dd:ee:ff", 1000, "milliseconds", 60_000);
        store.insert(session).await.unwrap();

        let updated = make_session("aa:bb:cc:dd:ee:ff", 2000, "milliseconds", 120_000);
        store
            .update("aa:bb:cc:dd:ee:ff", updated.clone())
            .await
            .unwrap();

        let got = store.get("aa:bb:cc:dd:ee:ff").await.unwrap();
        assert_eq!(got, Some(updated));
    }

    #[tokio::test]
    async fn sqlite_list_expired() {
        let store = SqliteSessionStore::open_in_memory().unwrap();

        let s1 = make_session("aa:bb:cc:dd:ee:ff", 100, "milliseconds", 60_000);
        store.insert(s1).await.unwrap();

        let s2 = make_session("11:22:33:44:55:66", 200, "milliseconds", 30_000);
        store.insert(s2).await.unwrap();

        let s3 = make_session("99:88:77:66:55:44", 100, "bytes", 1000);
        store.insert(s3).await.unwrap();

        let expired = store.list_expired(250).await.unwrap();
        assert_eq!(expired.len(), 2);

        let not_yet = store.list_expired(150).await.unwrap();
        assert_eq!(not_yet.len(), 0);
    }

    #[tokio::test]
    async fn sqlite_list_all() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        assert_eq!(store.list_all().await.unwrap(), [] as [CustomerSession; 0]);

        let s1 = make_session("aa:bb:cc:dd:ee:ff", 100, "milliseconds", 60_000);
        let s2 = make_session("11:22:33:44:55:66", 200, "milliseconds", 30_000);
        store.insert(s1.clone()).await.unwrap();
        store.insert(s2.clone()).await.unwrap();

        let mut all = store.list_all().await.unwrap();
        all.sort_by(|a, b| a.mac_address.cmp(&b.mac_address));
        assert_eq!(all, vec![s2, s1]);
    }
}

#[cfg(test)]
mod external_usage_tests {
    use super::tests::make_session;
    use super::*;

    fn snapshot(input: u64, output: u64, at: i64) -> super::ExternalUsageSnapshot {
        super::ExternalUsageSnapshot {
            input_octets: input,
            output_octets: output,
            session_time: 120,
            reported_at: at,
        }
    }

    #[tokio::test]
    async fn sqlite_persists_external_usage_snapshot() {
        let store = SqliteSessionStore::open_in_memory().unwrap();
        let mut session = make_session("aa:bb:cc:dd:ee:ff", 100, "bytes", 5000);
        session.last_external_usage = Some(snapshot(1000, 2000, 4242));
        store.insert(session).await.unwrap();

        let loaded = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(
            loaded.last_external_usage,
            Some(snapshot(1000, 2000, 4242)),
            "insert must persist the external-usage snapshot"
        );

        let mut updated = loaded;
        updated.last_external_usage = Some(snapshot(3000, 4000, 8484));
        updated.allotment = 9000;
        store.update("aa:bb:cc:dd:ee:ff", updated).await.unwrap();

        let reloaded = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(reloaded.allotment, 9000);
        assert_eq!(
            reloaded.last_external_usage,
            Some(snapshot(3000, 4000, 8484)),
            "update must overwrite the snapshot, not drop it"
        );
    }

    #[tokio::test]
    async fn sqlite_migrates_legacy_database_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE sessions (\
                 mac_address TEXT PRIMARY KEY, \
                 start_time  INTEGER NOT NULL, \
                 metric      TEXT    NOT NULL, \
                 allotment   INTEGER NOT NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('aa:bb:cc:dd:ee:ff', 100, 'milliseconds', 60000)",
                [],
            )
            .unwrap();
        }

        let store = SqliteSessionStore::open(path.to_str().unwrap()).unwrap();

        let loaded = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(
            loaded.allotment, 60_000,
            "legacy row must survive migration"
        );
        assert_eq!(
            loaded.last_external_usage, None,
            "legacy row has no snapshot until one is reported"
        );

        let mut updated = loaded;
        updated.last_external_usage = Some(snapshot(10, 20, 99));
        store.update("aa:bb:cc:dd:ee:ff", updated).await.unwrap();

        let reloaded = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(
            reloaded.last_external_usage,
            Some(snapshot(10, 20, 99)),
            "post-migration writes must persist snapshots"
        );
    }
}

#[cfg(test)]
mod atomicity_tests {
    use super::*;
    use std::sync::Arc;

    /// Eight overlapping top-ups for one MAC; returns the final allotment.
    async fn concurrent_topups(store: Arc<dyn SessionStore>) -> u64 {
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
        let results = futures::future::join_all(joins).await;
        results
            .into_iter()
            .map(|r| r.unwrap().allotment)
            .max()
            .unwrap()
    }

    #[tokio::test]
    async fn inmemory_overlapping_payments_are_all_counted() {
        let store = Arc::new(InMemorySessionStore::new());
        let final_allotment = concurrent_topups(store).await;
        assert_eq!(
            final_allotment,
            8 * 1_000,
            "every overlapping top-up must land (start_time keeps sessions unexpired)"
        );
    }

    #[tokio::test]
    async fn sqlite_overlapping_payments_are_all_counted() {
        let store = Arc::new(SqliteSessionStore::open_in_memory().unwrap());
        let final_allotment = concurrent_topups(store).await;
        assert_eq!(final_allotment, 8 * 1_000);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn merchant_provider_overlapping_payments_are_all_counted() {
        // Through the PUBLIC entry point the payment paths use — this is the
        // exact sequence two overlapping ln-invoice/bootstrap grants run.
        let store = Arc::new(InMemorySessionStore::new());
        let mut joins = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            joins.push(tokio::spawn(async move {
                crate::v1::server::merchant_provider::add_allotment(
                    &*store,
                    "aa:bb:cc:dd:ee:ff",
                    "milliseconds",
                    1_000,
                )
                .await
                .unwrap()
            }));
        }
        let results = futures::future::join_all(joins).await;
        let final_allotment = results
            .into_iter()
            .map(|r| r.unwrap().allotment)
            .max()
            .unwrap();
        assert_eq!(
            final_allotment,
            8 * 1_000,
            "every overlapping payment through add_allotment must land"
        );
    }

    #[tokio::test]
    async fn remove_expired_spares_a_session_renewed_after_the_snapshot() {
        let store = InMemorySessionStore::new();
        // Snapshot-era state: long expired (start 0, allotment 1s).
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 1_000, 0)
            .await
            .unwrap();

        // The renewal lands AFTER list_expired snapshotted it but BEFORE
        // the eviction: fresh start_time + big allotment.
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "milliseconds", 3_600_000, 10_000)
            .await
            .unwrap();

        let removed = store
            .remove_expired("aa:bb:cc:dd:ee:ff", 5_000)
            .await
            .unwrap();
        assert!(
            removed.is_none(),
            "a renewed session must survive a stale eviction attempt"
        );
        assert!(
            store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_some(),
            "the session must still be there"
        );

        // A genuinely still-expired session IS evicted.
        let gone = store
            .remove_expired("aa:bb:cc:dd:ee:ff", 10_000_000)
            .await
            .unwrap();
        assert!(gone.is_some(), "a truly expired session must be evicted");
        assert!(store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_exhausted_bytes_spares_a_topped_up_session() {
        let store = InMemorySessionStore::new();
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 1_000, 0)
            .await
            .unwrap();
        // Usage snapshot says exhausted (900 >= 1000? no — 1500 >= 1000 yes)...
        // top-up lands, current allotment 5_000 > usage 1_500.
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 4_000, 100)
            .await
            .unwrap();

        let removed = store
            .remove_exhausted_bytes("aa:bb:cc:dd:ee:ff", 1_500)
            .await
            .unwrap();
        assert!(
            removed.is_none(),
            "a topped-up session must survive a stale byte eviction"
        );
        assert!(store.get("aa:bb:cc:dd:ee:ff").await.unwrap().is_some());

        let gone = store
            .remove_exhausted_bytes("aa:bb:cc:dd:ee:ff", 9_999)
            .await
            .unwrap();
        assert!(
            gone.is_some(),
            "a truly exhausted byte session must be evicted"
        );
    }

    #[tokio::test]
    async fn external_usage_report_cannot_clobber_a_concurrent_topup() {
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
        assert_eq!(updated.last_external_usage, Some(snapshot.clone()));

        // A top-up after a report keeps the snapshot.
        store
            .add_allotment("aa:bb:cc:dd:ee:ff", "bytes", 3_000, 12_000)
            .await
            .unwrap();
        let after = store.get("aa:bb:cc:dd:ee:ff").await.unwrap().unwrap();
        assert_eq!(after.allotment, 8_000);
        assert_eq!(after.last_external_usage, Some(snapshot));
    }
}
