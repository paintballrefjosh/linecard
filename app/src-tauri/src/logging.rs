//! Session logs: what the terminal showed, as plain text (colors and cursor
//! control stripped), optionally with a timestamp on each line.
//!
//! The log setting (`log_path_template`) is either a folder, or a full path
//! template using %name %host %Y %m %d %H %M %S.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// File name used when the setting is just a folder.
pub const DEFAULT_FILE: &str = "%name_%Y-%m-%d_%H%M%S.log";

/// Characters Windows won't accept in a file name.
fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if r#"\/:*?"<>|"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Where a session's log goes. `setting` is the resolved `log_path_template`
/// (a folder or a template); `default_dir` is used when it's unset.
pub fn log_path(setting: Option<&str>, default_dir: &Path, name: &str, host: &str, now: DateTime<Local>) -> PathBuf {
    let setting = setting.map(str::trim).filter(|s| !s.is_empty());
    let template = match setting {
        Some(s) if s.contains('%') || s.to_ascii_lowercase().ends_with(".log") => PathBuf::from(s),
        Some(dir) => Path::new(dir).join(DEFAULT_FILE),
        None => default_dir.join(DEFAULT_FILE),
    };
    let text = template.to_string_lossy().into_owned();
    // Expand the file name only, so a folder containing '%' isn't mangled.
    let (dir, file) = match text.rfind(['\\', '/']) {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text.as_str()),
    };
    let file = file
        .replace("%name", &safe(name))
        .replace("%host", &safe(host))
        .replace("%Y", &now.format("%Y").to_string())
        .replace("%m", &now.format("%m").to_string())
        .replace("%d", &now.format("%d").to_string())
        .replace("%H", &now.format("%H").to_string())
        .replace("%M", &now.format("%M").to_string())
        .replace("%S", &now.format("%S").to_string());
    PathBuf::from(format!("{dir}{file}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Esc {
    None,
    /// After ESC.
    Start,
    /// CSI: ESC [ params... final byte
    Csi,
    /// OSC / DCS etc.: until BEL or ESC \
    String,
    StringEnd,
}

pub struct SessionLog {
    out: BufWriter<File>,

    timestamps: bool,
    line_start: bool,
    esc: Esc,
}

impl SessionLog {
    pub fn open(path: PathBuf, timestamps: bool) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(SessionLog {
            out: BufWriter::new(file),

            timestamps,
            line_start: true,
            esc: Esc::None,
        })
    }

    /// A line of our own, e.g. "session started".
    pub fn note(&mut self, text: &str) {
        if !self.line_start {
            let _ = self.out.write_all(b"\n");
        }
        let stamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(self.out, "=== {stamp} {text} ===");
        self.line_start = true;
        let _ = self.out.flush();
    }

    /// Terminal output as it arrived.
    pub fn write(&mut self, data: &[u8]) {
        let mut text = Vec::with_capacity(data.len());
        for &b in data {
            self.esc = match (self.esc, b) {
                (Esc::None, 0x1b) => Esc::Start,
                (Esc::None, b'\r' | 0x07 | 0x00) => Esc::None,
                (Esc::None, 0x08) => {
                    text.pop(); // backspace: drop the last character, as the screen does
                    Esc::None
                }
                (Esc::None, b) => {
                    if self.line_start && self.timestamps && b != b'\n' {
                        text.extend_from_slice(Local::now().format("[%H:%M:%S] ").to_string().as_bytes());
                    }
                    self.line_start = b == b'\n';
                    text.push(b);
                    Esc::None
                }
                (Esc::Start, b'[') => Esc::Csi,
                (Esc::Start, b']' | b'P' | b'X' | b'^' | b'_') => Esc::String,
                (Esc::Start, _) => Esc::None, // two-byte sequence
                (Esc::Csi, 0x40..=0x7e) => Esc::None,
                (Esc::Csi, _) => Esc::Csi,
                (Esc::String, 0x07) => Esc::None,
                (Esc::String, 0x1b) => Esc::StringEnd,
                (Esc::String, _) => Esc::String,
                (Esc::StringEnd, b'\\') => Esc::None,
                (Esc::StringEnd, _) => Esc::String,
            };
        }
        let _ = self.out.write_all(&text);
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 28, 9, 5, 7).unwrap()
    }

    #[test]
    fn paths_from_folders_and_templates() {
        let def = Path::new(r"C:\Users\me\Documents\Linecard Logs");
        assert_eq!(
            log_path(None, def, "core/sw:1", "10.0.0.1", at()),
            def.join("core_sw_1_2026-10-28_090507.log")
        );
        assert_eq!(
            log_path(Some(r"D:\logs"), def, "sw1", "h", at()),
            PathBuf::from(r"D:\logs").join("sw1_2026-10-28_090507.log")
        );
        assert_eq!(
            log_path(Some(r"D:\100%\%host-%Y%m%d.log"), def, "sw1", "10.0.0.1", at()),
            PathBuf::from(r"D:\100%\10.0.0.1-20261028.log"),
            "only the file name is expanded"
        );
    }

    #[test]
    fn logs_are_plain_text() {
        let dir = std::env::temp_dir().join(format!("linecard-log-test-{}", std::process::id()));
        let path = dir.join("t.log");
        let _ = fs::remove_dir_all(&dir);
        let mut log = SessionLog::open(path.clone(), false).unwrap();
        // Colors, a title (OSC), cursor moves split across writes, CRLF, backspace.
        log.write(b"\x1b[32mconn");
        log.write(b"ected\x1b[0m\r\n\x1b]0;sw1\x07sw1# shw\x08ow ver\x1b[");
        log.write(b"2K\r\n");
        drop(log);
        assert_eq!(fs::read_to_string(&path).unwrap(), "connected\nsw1# show ver\n");
    }

    #[test]
    fn timestamps_start_each_line() {
        let dir = std::env::temp_dir().join(format!("linecard-log-ts-{}", std::process::id()));
        let path = dir.join("t.log");
        let _ = fs::remove_dir_all(&dir);
        let mut log = SessionLog::open(path.clone(), true).unwrap();
        log.write(b"a\r\nb");
        drop(log);
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with('[') && l.len() == "[00:00:00] a".len()),
            "{text}"
        );
    }
}
