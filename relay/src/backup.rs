//! Backups: a consistent copy of the database taken while the relay serves,
//! checked after it is made, and a restore that checks before it replaces.
//!
//! Everything in the file is already end-to-end encrypted (it also holds
//! hashes of the groups' tokens and of account keys), so a backup needs no
//! extra encryption, only to be kept private.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};

use super::{now_ms, Relay, DAY_MS, SCHEMA_VERSION};

/// Backups made by the relay are named `linecard-relay-YYYYMMDD-HHMMSS.db`.
const PREFIX: &str = "linecard-relay-";
const SUFFIX: &str = ".db";

/// Seconds since the epoch → "YYYYMMDD-HHMMSS" in UTC.
pub fn stamp(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn err(msg: impl std::fmt::Display) -> String {
    msg.to_string()
}

/// What a backup file holds.
#[derive(Debug, PartialEq, Eq)]
pub struct BackupInfo {
    pub schema: i64,
    pub groups: i64,
    pub accounts: i64,
    pub bytes: u64,
}

/// Open `path` read-only and check that it is a sound relay database from
/// this version of the relay or an older one.
pub fn check(path: &Path) -> Result<BackupInfo, String> {
    let bytes = fs::metadata(path)
        .map_err(|e| format!("can't read {}: {e}", path.display()))?
        .len();
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("{} is not a database: {e}", path.display()))?;
    let verdict: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("{} is damaged: {e}", path.display()))?;
    if verdict != "ok" {
        return Err(format!("{} is damaged: {verdict}", path.display()));
    }
    let schema: i64 = db
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))
        .map_err(|_| format!("{} is not a Linecard relay database", path.display()))?;
    if schema == 0 {
        return Err(format!("{} is not a Linecard relay database", path.display()));
    }
    if schema > SCHEMA_VERSION {
        return Err(format!(
            "{} is from a newer relay (schema {schema}; this one knows {SCHEMA_VERSION}): use that version to restore it",
            path.display()
        ));
    }
    let count = |table: &str| db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0));
    Ok(BackupInfo {
        schema,
        groups: count("groups").map_err(err)?,
        accounts: count("accounts").map_err(err)?,
        bytes,
    })
}

impl Relay {
    /// Copy the database to `dest` while the relay keeps serving. The copy is
    /// a single standalone file, verified before it is put in place; `dest` is
    /// replaced only when the new copy is good.
    pub fn backup(&self, dest: &Path) -> Result<BackupInfo, String> {
        let tmp = dest.with_extension("db.partial");
        let _ = fs::remove_file(&tmp);
        let made = (|| {
            // A second connection reads the file, so requests aren't held up
            // for the length of the copy (WAL lets readers and the writer coexist).
            let own;
            let guard;
            let source: &Connection = match &self.path {
                Some(p) => {
                    own = Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
                    own.busy_timeout(Duration::from_secs(5)).map_err(err)?;
                    &own
                }
                None => {
                    guard = self.db().map_err(|_| "database lock poisoned".to_string())?;
                    &guard
                }
            };
            let mut out = Connection::open(&tmp).map_err(|e| format!("can't create {}: {e}", tmp.display()))?;
            Backup::new(source, &mut out)
                .and_then(|b| b.run_to_completion(256, Duration::from_millis(2), None))
                .map_err(|e| format!("copy failed: {e}"))?;
            // A standalone file: no -wal or -shm beside it.
            out.pragma_update(None, "journal_mode", "DELETE").map_err(err)?;
            drop(out);
            check(&tmp)
        })();
        match made {
            Ok(info) => {
                fs::rename(&tmp, dest).map_err(|e| format!("can't write {}: {e}", dest.display()))?;
                Ok(BackupInfo {
                    bytes: fs::metadata(dest).map(|m| m.len()).unwrap_or(info.bytes),
                    ..info
                })
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// A backup named for now inside `dir`, then delete the oldest relay
    /// backups there beyond the newest `keep`. Does nothing when the newest
    /// one is under 20 hours old (a restart shouldn't add a copy each time).
    /// Returns the new file, or `None` if it was skipped.
    pub fn backup_rotating(&self, dir: &Path, keep: usize) -> Result<Option<PathBuf>, String> {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        let existing = list_backups(dir);
        // File names carry their UTC time and sort by it, so "newer than 20 hours
        // ago" is a string comparison (and survives the files being copied around).
        let cutoff = stamp(now_ms() - DAY_MS * 20 / 24);
        let fresh = existing
            .last()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|n| {
                n.strip_prefix(PREFIX)
                    .and_then(|n| n.strip_suffix(SUFFIX))
                    .is_some_and(|s| s >= cutoff.as_str())
            });
        if fresh {
            return Ok(None);
        }
        let file = dir.join(format!("{PREFIX}{}{SUFFIX}", stamp(now_ms())));
        self.backup(&file)?;
        prune(dir, keep);
        Ok(Some(file))
    }
}

/// The relay-made backups in `dir`, oldest first (their names sort by time).
pub fn list_backups(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
        })
        .collect();
    files.sort();
    files
}

/// Delete all but the newest `keep` backups in `dir` (at least one is kept).
pub fn prune(dir: &Path, keep: usize) {
    let files = list_backups(dir);
    let extra = files.len().saturating_sub(keep.max(1));
    for old in &files[..extra] {
        let _ = fs::remove_file(old);
    }
}

/// Replace the database at `db` with the backup at `from`. The relay must be
/// stopped. The old file is moved aside (`<db>.before-restore-<time>`), never
/// deleted, and the backup is checked first. Returns what was restored and
/// where the old database went.
pub fn restore(from: &Path, db: &Path) -> Result<(BackupInfo, Option<PathBuf>), String> {
    let info = check(from)?;
    if let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let tmp = db.with_extension("db.restoring");
    fs::copy(from, &tmp).map_err(|e| format!("can't copy the backup: {e}"))?;
    // The relay's history just went back in time: a new epoch tells the
    // computers syncing with it to send theirs again.
    {
        let copy = Connection::open(&tmp).map_err(err)?;
        copy.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .map_err(err)?;
        super::set_new_epoch(&copy).map_err(err)?;
    }
    let aside = if db.exists() {
        let mut name = db.as_os_str().to_owned();
        name.push(format!(".before-restore-{}", stamp(now_ms())));
        let aside = PathBuf::from(name);
        fs::rename(db, &aside).map_err(|e| format!("can't move the current database aside: {e}"))?;
        Some(aside)
    } else {
        None
    };
    // The old database's write-ahead log would corrupt the restored one.
    for ext in ["-wal", "-shm"] {
        let mut leftover = db.as_os_str().to_owned();
        leftover.push(ext);
        let _ = fs::remove_file(PathBuf::from(leftover));
    }
    fs::rename(&tmp, db).map_err(|e| format!("can't put the backup in place: {e}"))?;
    Ok((info, aside))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;

    const G: &str = "0123456789abcdef0123456789abcdef";
    const T: &str = "token-for-the-group-xyz";

    fn temp_dir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("linecard-relay-bk-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_709_164_800_000 + 3_661_000), "20240229-010101"); // a leap day
        assert_eq!(stamp(1_700_000_000_000), "20231114-221320");
        assert!(stamp(1_700_000_000_000) < stamp(1_800_000_000_000));
    }

    #[test]
    fn a_backup_taken_while_serving_restores_everything() {
        let dir = temp_dir("roundtrip");
        let db = dir.join("relay.db");
        let relay = Relay::open(&db, Limits::default()).unwrap();
        let key = relay.add_account("alice", Some(3)).unwrap();
        relay.push(G, T, Some(&key), "laptop", "first").unwrap();
        relay.push(G, T, Some(&key), "laptop", "second").unwrap();
        relay.put_device(G, T, None, "laptop", "hb").unwrap();

        let copy = dir.join("copy.db");
        let epoch_before = relay.epoch().to_string();
        let info = relay.backup(&copy).unwrap();
        assert_eq!((info.schema, info.groups, info.accounts), (SCHEMA_VERSION, 1, 1));
        assert!(!dir.join("copy.db.partial").exists());
        assert!(!dir.join("copy.db-wal").exists(), "a standalone file");

        // The relay carries on after the backup, and the backup is a moment in time.
        relay.push(G, T, Some(&key), "laptop", "third").unwrap();
        assert_eq!(check(&copy).unwrap().groups, 1);
        drop(relay);

        // Lose the database, restore, and everything is back, accounts and tokens too.
        let (restored, aside) = restore(&copy, &db).unwrap();
        assert_eq!(restored.groups, 1);
        assert!(aside.is_some_and(|a| a.exists()), "the old file is kept, not deleted");
        let again = Relay::open(&db, Limits::default()).unwrap();
        assert_ne!(again.epoch(), epoch_before, "a restore starts a new epoch");
        let ops = again.pull(G, T, 0).unwrap().0;
        assert_eq!(
            ops.iter().map(|o| o.blob.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(again.devices(G, T).unwrap().len(), 1);
        assert_eq!(again.accounts().unwrap()[0].name, "alice");
        assert_eq!(
            again.push(G, "someone-elses-token-xyz", None, "x", "y"),
            Err(crate::RelayError::Unauthorized)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backups_keep_working_under_concurrent_writes() {
        let dir = temp_dir("busy");
        let relay = std::sync::Arc::new(Relay::open(&dir.join("relay.db"), Limits::default()).unwrap());
        let writer = {
            let relay = relay.clone();
            std::thread::spawn(move || {
                for i in 0..300 {
                    relay
                        .push(G, T, None, "d", &format!("batch {i} {}", "x".repeat(2000)))
                        .unwrap();
                }
            })
        };
        let mut taken = 0;
        while !writer.is_finished() {
            let info = relay.backup(&dir.join(format!("b{taken}.db"))).unwrap();
            assert_eq!(info.groups.min(1), info.groups); // never a half-made group table
            taken += 1;
        }
        writer.join().unwrap();
        assert!(taken > 0);
        // Each copy is internally consistent: its batches are 1..=n with no gaps,
        // and its byte count matches what it holds.
        for i in 0..taken {
            let db = Connection::open(dir.join(format!("b{i}.db"))).unwrap();
            let (n, max, sum): (i64, Option<i64>, i64) = db
                .query_row(
                    "SELECT COUNT(*), MAX(seq), COALESCE(SUM(LENGTH(blob)), 0) FROM ops",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(max.unwrap_or(0), n);
            let counted: i64 = db
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM groups", [], |r| r.get(0))
                .unwrap();
            assert_eq!(counted, sum);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_refuses_files_that_are_not_good_backups() {
        let dir = temp_dir("refuse");
        let db = dir.join("relay.db");
        let relay = Relay::open(&db, Limits::default()).unwrap();
        relay.push(G, T, None, "d", "keep me").unwrap();
        drop(relay);

        let junk = dir.join("junk.db");
        fs::write(&junk, b"this is not a database at all, just some text").unwrap();
        assert!(restore(&junk, &db).is_err());
        let other = dir.join("other.db");
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert!(restore(&other, &db)
            .unwrap_err()
            .contains("not a Linecard relay database"));
        assert!(restore(&dir.join("missing.db"), &db).is_err());
        let newer = dir.join("newer.db");
        fs::copy(&db, &newer).unwrap();
        Connection::open(&newer)
            .unwrap()
            .execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION + 1])
            .unwrap();
        assert!(restore(&newer, &db).unwrap_err().contains("newer relay"));

        // The live database was never touched.
        let relay = Relay::open(&db, Limits::default()).unwrap();
        assert_eq!(relay.pull(G, T, 0).unwrap().0.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_names_by_time_keeps_the_newest_and_skips_fresh_ones() {
        let dir = temp_dir("rotate");
        let relay = Relay::open(&dir.join("relay.db"), Limits::default()).unwrap();
        relay.push(G, T, None, "d", "x").unwrap();
        let backups = dir.join("backups");

        // Old backups from earlier days, plus an unrelated file that must stay.
        fs::create_dir_all(&backups).unwrap();
        for day in ["20240101", "20240102", "20240103", "20240104"] {
            fs::write(backups.join(format!("linecard-relay-{day}-030000.db")), b"old").unwrap();
        }
        fs::write(backups.join("notes.txt"), b"mine").unwrap();
        // The newest is from 2024, so a new one is due.
        let made = relay.backup_rotating(&backups, 3).unwrap().expect("a new backup");
        assert!(made
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("linecard-relay-"));
        let names: Vec<String> = list_backups(&backups)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert_eq!(names[0], "linecard-relay-20240103-030000.db");
        assert!(backups.join("notes.txt").exists());
        assert_eq!(check(&made).unwrap().groups, 1);

        // Just made one: asking again right away does nothing.
        assert_eq!(relay.backup_rotating(&backups, 3).unwrap(), None);
        assert_eq!(list_backups(&backups).len(), 3);

        // `prune` never removes the last backup, whatever `keep` says.
        prune(&backups, 0);
        assert_eq!(list_backups(&backups).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
