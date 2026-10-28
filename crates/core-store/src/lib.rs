//! Local SQLite store.
//!
//! The op log is the source of truth: on startup the app replays it into a
//! [`Replica`]. Snapshots keep startup fast once the log grows (Phase 2).
//! Ack state and local-only settings are stored here too and never synced.

use std::path::Path;

use linecard_core_sync::{Hlc, Op, Replica};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("corrupt op record: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Ordered list of schema migrations. Append only; never edit a shipped one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "CREATE TABLE ops (
        wall_ms   INTEGER NOT NULL,
        counter   INTEGER NOT NULL,
        device    TEXT    NOT NULL,
        object    TEXT    NOT NULL,
        body      TEXT    NOT NULL,       -- JSON-encoded Op
        PRIMARY KEY (wall_ms, counter, device)
     ) WITHOUT ROWID;
     CREATE INDEX ops_object ON ops(object);
     CREATE TABLE sync_cursor (
        transport TEXT PRIMARY KEY,       -- 'relay', 'git', 'lan'
        cursor    TEXT NOT NULL           -- transport-specific position
     );
     CREATE TABLE local_settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );",
];

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Store { db };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: usize = self.db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tx = self.db.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i + 1)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Append ops (local or remote). Duplicates are ignored.
    pub fn append_ops<'a>(&mut self, ops: impl IntoIterator<Item = &'a Op>) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO ops (wall_ms, counter, device, object, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for op in ops {
                stmt.execute(params![
                    op.id.wall_ms as i64,
                    op.id.counter,
                    op.id.device,
                    op.object,
                    serde_json::to_string(op)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All ops in timestamp order.
    pub fn load_ops(&self) -> Result<Vec<Op>> {
        let mut stmt = self
            .db
            .prepare("SELECT body FROM ops ORDER BY wall_ms, counter, device")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for body in rows {
            out.push(serde_json::from_str(&body?)?);
        }
        Ok(out)
    }

    /// Rebuild the in-memory replica from the log. Replayed ops are not
    /// highlighted: the device has already seen them.
    pub fn load_replica(&self, device: &str, now_ms: u64) -> Result<Replica> {
        let mut replica = Replica::new(device);
        for op in self.load_ops()? {
            replica.apply_remote(op, now_ms);
        }
        replica.changes.ack_all();
        Ok(replica)
    }

    pub fn latest_op(&self) -> Result<Option<Hlc>> {
        Ok(self
            .db
            .query_row(
                "SELECT wall_ms, counter, device FROM ops ORDER BY wall_ms DESC, counter DESC, device DESC LIMIT 1",
                [],
                |r| {
                    Ok(Hlc {
                        wall_ms: r.get::<_, i64>(0)? as u64,
                        counter: r.get(1)?,
                        device: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn cursor(&self, transport: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT cursor FROM sync_cursor WHERE transport = ?1",
                [transport],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_cursor(&self, transport: &str, cursor: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO sync_cursor (transport, cursor) VALUES (?1, ?2)
             ON CONFLICT(transport) DO UPDATE SET cursor = excluded.cursor",
            params![transport, cursor],
        )?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM local_settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO local_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_sync::Value;
    use std::collections::BTreeMap;

    #[test]
    fn ops_round_trip_and_replay() {
        let mut store = Store::open_in_memory().unwrap();
        let mut r = Replica::new("A");
        let c = r.create(
            "c1",
            "connection",
            BTreeMap::from([("host".into(), Value::from("10.0.0.1"))]),
            100,
        );
        let u = r
            .update("c1", BTreeMap::from([("host".into(), Value::from("10.0.0.2"))]), 200)
            .unwrap();
        store.append_ops([&c, &u, &c]).unwrap();
        assert_eq!(store.load_ops().unwrap().len(), 2);
        assert_eq!(store.latest_op().unwrap(), Some(u.id.clone()));

        let replayed = store.load_replica("A", 300).unwrap();
        assert_eq!(
            replayed.object("c1").unwrap().get("host"),
            Some(&Value::from("10.0.0.2"))
        );
        assert!(replayed.changes.is_empty());
    }

    #[test]
    fn cursors_and_settings() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.cursor("relay").unwrap(), None);
        store.set_cursor("relay", "42").unwrap();
        store.set_cursor("relay", "43").unwrap();
        assert_eq!(store.cursor("relay").unwrap().as_deref(), Some("43"));
        store.set_setting("theme", "system").unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("system"));
    }
}
