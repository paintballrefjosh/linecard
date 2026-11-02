//! Linecard's own debug log: what went wrong in sync, remote sessions and
//! updates, in one small file to attach to a bug report.
//!
//! Never holds terminal output, passwords or keys; only short error
//! messages with the connection's kind and address. The file rotates at
//! [`MAX_BYTES`], keeping one older copy.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;

const MAX_BYTES: u64 = 512 * 1024;
pub const FILE: &str = "linecard-debug.log";

struct Log {
    path: PathBuf,
    /// The last line written per area, so a retry loop doesn't flood the file.
    last: Vec<(String, String)>,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Start logging to `dir`; writes a header with the app and system.
pub fn init(dir: &std::path::Path, version: &str) {
    let _ = fs::create_dir_all(dir);
    let path = dir.join(FILE);
    if let Ok(mut g) = LOG.lock() {
        *g = Some(Log { path, last: Vec::new() });
    }
    write(
        "app",
        &format!(
            "Linecard {version} on {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
}

/// Record `message` under `area` (e.g. "sync", "rdp", "update"). An exact
/// repeat of the area's previous message is skipped.
pub fn write(area: &str, message: &str) {
    let Ok(mut g) = LOG.lock() else { return };
    let Some(log) = g.as_mut() else { return };
    let message = message.trim().replace(['\r', '\n'], " ");
    match log.last.iter_mut().find(|(a, _)| a == area) {
        Some((_, m)) if *m == message => return,
        Some((_, m)) => *m = message.clone(),
        None => log.last.push((area.to_string(), message.clone())),
    }
    if fs::metadata(&log.path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(&log.path, log.path.with_extension("log.1"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log.path) {
        let _ = writeln!(f, "{} [{area}] {message}", Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

/// The log as text, older part first (empty if nothing was logged).
pub fn read() -> String {
    let Ok(g) = LOG.lock() else { return String::new() };
    let Some(log) = g.as_ref() else { return String::new() };
    let old = fs::read_to_string(log.path.with_extension("log.1")).unwrap_or_default();
    old + &fs::read_to_string(&log.path).unwrap_or_default()
}

pub fn path() -> Option<PathBuf> {
    LOG.lock().ok()?.as_ref().map(|l| l.path.clone())
}

/// Send the RDP library's own notes about sound and dynamic channels to the
/// log (area "rdp-trace"), to see how a server negotiates them. Only those
/// two parts of the library: they carry channel names and audio formats.
pub fn trace() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    struct Line;
    impl Write for Line {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            write("rdp-trace", &String::from_utf8_lossy(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let only = Targets::new()
        .with_target("ironrdp_rdpsnd", LevelFilter::DEBUG)
        .with_target("ironrdp_dvc", LevelFilter::DEBUG)
        .with_target("ironrdp_connector::channel_connection", LevelFilter::DEBUG);
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(|| Line)
        .with_filter(only);
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_dedupes_repeats_and_rotates() {
        let dir = std::env::temp_dir().join(format!("linecard-debuglog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        init(&dir, "0.0.0");
        write("sync", "relay unreachable");
        write("sync", "relay unreachable"); // repeat: skipped
        write("rdp", "relay unreachable"); // another area: kept
        write("sync", "back\nonline");
        let text = read();
        assert_eq!(text.matches("relay unreachable").count(), 2, "{text}");
        assert!(text.contains("[sync] back online"), "{text}");
        assert!(text.lines().next().unwrap().contains("[app] Linecard 0.0.0"));

        // Past the cap, the current file becomes the older copy.
        fs::write(dir.join(FILE), vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        write("sync", "after rotation");
        assert!(dir.join("linecard-debug.log.1").exists());
        assert!(fs::read_to_string(dir.join(FILE)).unwrap().contains("after rotation"));
        let _ = fs::remove_dir_all(&dir);
    }
}
