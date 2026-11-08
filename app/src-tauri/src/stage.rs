//! Files dropped onto the SFTP browser, staged on disk.
//!
//! The app turns off the window's native drag-and-drop so that tabs can be
//! dragged, which means a dropped file reaches the page as data without a
//! path. The page sends it here in pieces; it is written under a temporary
//! folder, uploaded like any other local file, and the folder is removed.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tauri::ipc::{InvokeBody, Request};

use crate::{err, CmdResult};

const PREFIX: &str = "linecard-drop-";

/// Folders handed out and not yet removed; only these accept writes.
fn live() -> &'static Mutex<HashSet<PathBuf>> {
    static LIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// `rel` ("dir/sub/file.txt") under `base`, refusing anything that could
/// land outside it.
fn inside(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut out = base.to_path_buf();
    let mut any = false;
    for part in rel.split('/') {
        if part.is_empty() {
            continue;
        }
        let ok = matches!(
            Path::new(part).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        ) && !part.contains(['\\', ':']);
        if !ok {
            return Err(format!("not a usable file name: {part}"));
        }
        out.push(part);
        any = true;
    }
    if any {
        Ok(out)
    } else {
        Err("no file name".into())
    }
}

/// A staging folder is one of ours.
fn owned(dir: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(dir);
    match live().lock() {
        Ok(set) if set.contains(&path) => Ok(path),
        _ => Err("unknown staging folder".into()),
    }
}

/// Remove staging folders left by a crash, once they are a day old.
fn sweep(temp: &Path) {
    let Ok(read) = fs::read_dir(temp) else { return };
    for e in read.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old && e.file_name().to_string_lossy().starts_with(PREFIX) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// A new empty staging folder; returns its path.
#[tauri::command]
pub fn stage_begin() -> CmdResult<String> {
    let temp = std::env::temp_dir();
    sweep(&temp);
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = temp.join(format!(
        "{PREFIX}{}-{nanos:x}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).map_err(err)?;
    live().lock().map_err(err)?.insert(dir.clone());
    Ok(dir.to_string_lossy().into_owned())
}

/// An (empty) folder `rel` inside the staging folder.
#[tauri::command]
pub fn stage_mkdir(dir: String, rel: String) -> CmdResult<()> {
    let path = inside(&owned(&dir)?, &rel)?;
    fs::create_dir_all(path).map_err(err)
}

/// Write the request body into file `x-rel` of staging folder `x-dir`, at
/// byte `x-offset` (0 starts the file afresh).
#[tauri::command]
pub fn stage_write(request: Request<'_>) -> CmdResult<()> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw bytes".into());
    };
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string())
            .ok_or_else(|| format!("missing {name}"))
    };
    let decode = |s: String| percent_decode(&s).ok_or_else(|| "bad escape in a header".to_string());
    let dir = decode(header("x-dir")?)?;
    let rel = decode(header("x-rel")?)?;
    let offset: u64 = header("x-offset")?.parse().map_err(err)?;
    let path = inside(&owned(&dir)?, &rel)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(err)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(path)
        .map_err(err)?;
    f.seek(SeekFrom::Start(offset)).map_err(err)?;
    f.write_all(bytes).map_err(err)
}

/// Remove a staging folder and everything in it.
#[tauri::command]
pub fn stage_end(dir: String) -> CmdResult<()> {
    let path = owned(&dir)?;
    live().lock().map_err(err)?.remove(&path);
    fs::remove_dir_all(path).map_err(err)
}

/// `encodeURIComponent` in reverse.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_staging_folder() {
        let base = Path::new("/stage");
        assert_eq!(inside(base, "a/b.txt").unwrap(), base.join("a").join("b.txt"));
        assert_eq!(inside(base, "/a//b").unwrap(), base.join("a").join("b"));
        for bad in ["../x", "a/../../x", "..", "", "/", "C:/x", "a\\b", "a/./b/.."] {
            assert!(inside(base, bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn headers_decode_like_encode_uri_component() {
        assert_eq!(percent_decode("a%20b%2Fc%C3%A9").as_deref(), Some("a b/c\u{e9}"));
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        assert!(percent_decode("%zz").is_none());
        assert!(percent_decode("%4").is_none());
    }

    #[test]
    fn only_folders_we_made_accept_writes() {
        assert!(owned("/tmp/somewhere-else").is_err());
        let dir = stage_begin().unwrap();
        assert!(owned(&dir).is_ok());
        stage_mkdir(dir.clone(), "empty/inner".into()).unwrap();
        assert!(Path::new(&dir).join("empty").join("inner").is_dir());
        stage_end(dir.clone()).unwrap();
        assert!(!Path::new(&dir).exists());
        assert!(owned(&dir).is_err(), "gone");
    }
}
