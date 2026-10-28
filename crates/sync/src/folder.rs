//! Sync through a shared folder: a NAS share, OneDrive, Dropbox, a USB stick.
//!
//! ```text
//! <folder>/linecard-<group id>/
//!   ops/<device>/000000000001.json     one sealed batch per file
//!   devices/<device>.json              latest heartbeat
//! ```
//!
//! Every device writes only under its own name, so two devices never touch
//! the same file and cloud-sync conflict copies can't happen. Files are
//! written to a temporary name and renamed, so a reader never sees half a
//! file. The cursor is the last sequence number read from each device.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{Received, Sealed, SyncError};

pub type FolderCursor = BTreeMap<String, u64>;

pub struct FolderTransport {
    root: PathBuf,
    device: String,
}

/// Device ids end up in file names; keep them to safe characters.
fn safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SyncError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn seq_of(name: &str) -> Option<u64> {
    name.strip_suffix(".json")?.parse().ok()
}

fn modified_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl FolderTransport {
    pub fn new(folder: impl Into<PathBuf>, group_id: &str, device: &str) -> Self {
        FolderTransport {
            root: folder.into().join(format!("linecard-{group_id}")),
            device: safe(device),
        }
    }

    /// Whether the chosen folder is reachable at all (e.g. a mapped drive
    /// that's offline).
    pub fn check(&self) -> Result<(), SyncError> {
        let parent = self.root.parent().unwrap_or(&self.root);
        if !parent.is_dir() {
            return Err(SyncError::Storage(format!("{} isn't reachable", parent.display())));
        }
        fs::create_dir_all(self.root.join("ops"))?;
        Ok(())
    }

    fn own_dir(&self) -> PathBuf {
        self.root.join("ops").join(&self.device)
    }

    fn files_after(dir: &Path, after: u64) -> Result<Vec<(u64, PathBuf)>, SyncError> {
        let mut files = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(files);
        };
        for e in entries.flatten() {
            if let Some(seq) = seq_of(&e.file_name().to_string_lossy()) {
                if seq > after {
                    files.push((seq, e.path()));
                }
            }
        }
        files.sort();
        Ok(files)
    }

    pub fn push(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let dir = self.own_dir();
        let next = Self::files_after(&dir, 0)?.last().map_or(1, |(s, _)| s + 1);
        let bytes = serde_json::to_vec(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        write_atomic(&dir.join(format!("{next:012}.json")), &bytes)
    }

    /// Everything other devices wrote since `cursor`; returns the new cursor.
    pub fn pull(&self, cursor: &FolderCursor) -> Result<(Vec<Received>, FolderCursor), SyncError> {
        let mut out = Vec::new();
        let mut next = cursor.clone();
        let Ok(devices) = fs::read_dir(self.root.join("ops")) else {
            return Ok((out, next));
        };
        for d in devices.flatten() {
            let name = d.file_name().to_string_lossy().into_owned();
            if name == self.device || !d.path().is_dir() {
                continue;
            }
            let after = cursor.get(&name).copied().unwrap_or(0);
            for (seq, path) in Self::files_after(&d.path(), after)? {
                // A file still syncing in from the cloud may be partial: stop
                // at it and try again next time (order matters per device).
                let Some(sealed) = fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Sealed>(&b).ok())
                else {
                    break;
                };
                out.push(Received {
                    stored_at: modified_ms(&path),
                    sealed,
                });
                next.insert(name.clone(), seq);
            }
        }
        Ok((out, next))
    }

    pub fn put_device(&self, sealed: &Sealed) -> Result<(), SyncError> {
        let bytes = serde_json::to_vec(sealed).map_err(|e| SyncError::Storage(e.to_string()))?;
        write_atomic(&self.root.join("devices").join(format!("{}.json", self.device)), &bytes)
    }

    pub fn devices(&self) -> Result<Vec<Received>, SyncError> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.root.join("devices")) else {
            return Ok(out);
        };
        for e in entries.flatten() {
            if !e.file_name().to_string_lossy().ends_with(".json") {
                continue;
            }
            if let Some(sealed) = fs::read(e.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Sealed>(&b).ok())
            {
                out.push(Received {
                    stored_at: modified_ms(&e.path()),
                    sealed,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{open, seal, DeviceKey, GroupKey};

    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "linecard-folder-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn two_devices_exchange_batches_in_order() {
        let dir = temp();
        let group = GroupKey::generate();
        let (ka, kb) = (DeviceKey::generate(), DeviceKey::generate());
        let a = FolderTransport::new(&dir, &group.group_id(), "laptop");
        let b = FolderTransport::new(&dir, &group.group_id(), "desktop");
        a.check().unwrap();

        for n in 1..=3 {
            a.push(&seal(&group, &ka, "laptop", &n)).unwrap();
        }
        b.push(&seal(&group, &kb, "desktop", &100)).unwrap();

        // B sees A's three batches, in order, and not its own.
        let (got, cursor) = b.pull(&FolderCursor::new()).unwrap();
        let nums: Vec<i32> = got.iter().map(|r| open(&group, &r.sealed).unwrap()).collect();
        assert_eq!(nums, vec![1, 2, 3]);
        assert_eq!(cursor.get("laptop"), Some(&3));

        // Nothing new until A writes again.
        assert!(b.pull(&cursor).unwrap().0.is_empty());
        a.push(&seal(&group, &ka, "laptop", &4)).unwrap();
        let (got, _) = b.pull(&cursor).unwrap();
        assert_eq!(got.len(), 1);

        // A sees B's.
        let (got, _) = a.pull(&FolderCursor::new()).unwrap();
        assert_eq!(open::<i32>(&group, &got[0].sealed).unwrap(), 100);
    }

    #[test]
    fn partial_files_wait_for_the_next_pull() {
        let dir = temp();
        let group = GroupKey::generate();
        let k = DeviceKey::generate();
        let a = FolderTransport::new(&dir, &group.group_id(), "laptop");
        let b = FolderTransport::new(&dir, &group.group_id(), "desktop");
        a.push(&seal(&group, &k, "laptop", &1)).unwrap();
        // Simulate file 2 still downloading from the cloud, then file 3.
        let ops = dir
            .join(format!("linecard-{}", group.group_id()))
            .join("ops")
            .join("laptop");
        fs::write(ops.join("000000000002.json"), b"{\"v\":1,\"dev").unwrap();
        a.push(&seal(&group, &k, "laptop", &3)).unwrap();

        let (got, cursor) = b.pull(&FolderCursor::new()).unwrap();
        assert_eq!(got.len(), 1, "stops before the partial file");
        assert_eq!(cursor.get("laptop"), Some(&1));
        fs::write(
            ops.join("000000000002.json"),
            serde_json::to_vec(&seal(&group, &k, "laptop", &2)).unwrap(),
        )
        .unwrap();
        let (got, cursor) = b.pull(&cursor).unwrap();
        let nums: Vec<i32> = got.iter().map(|r| open(&group, &r.sealed).unwrap()).collect();
        assert_eq!(nums, vec![2, 3]);
        assert_eq!(cursor.get("laptop"), Some(&3));
    }

    #[test]
    fn heartbeats_and_unreachable_folders() {
        let dir = temp();
        let group = GroupKey::generate();
        let k = DeviceKey::generate();
        let a = FolderTransport::new(&dir, &group.group_id(), "laptop");
        a.put_device(&seal(&group, &k, "laptop", &"hello")).unwrap();
        a.put_device(&seal(&group, &k, "laptop", &"again")).unwrap();
        let devs = a.devices().unwrap();
        assert_eq!(devs.len(), 1, "one file per device, replaced");
        assert_eq!(open::<String>(&group, &devs[0].sealed).unwrap(), "again");

        let gone = FolderTransport::new(dir.join("no/such/share"), &group.group_id(), "x");
        assert!(gone.check().is_err());
    }
}
