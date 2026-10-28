//! SecureCRT session import.
//!
//! SecureCRT keeps one `.ini` file per session under `Config/Sessions/`, with
//! sub-directories as folders. Lines look like:
//!
//! ```text
//! S:"Hostname"=10.1.1.1
//! S:"Protocol Name"=SSH2
//! D:"[SSH2] Port"=00000016
//! ```
//!
//! `S:` is a string, `D:` a hex DWORD. `__FolderData__.ini` and `Default.ini`
//! are metadata, not sessions. Saved passwords are encrypted with SecureCRT's
//! own scheme and are not imported.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use linecard_core_model::{ConnectionKind, Endpoint, FlowControl, Parity, SerialSettings};

use crate::{Import, ImportedConnection};

/// Parsed key/value pairs from one SecureCRT ini file.
#[derive(Debug, Default)]
pub struct IniValues {
    strings: HashMap<String, String>,
    dwords: HashMap<String, u32>,
}

impl IniValues {
    pub fn parse(text: &str) -> Self {
        let mut v = IniValues::default();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let Some((kind, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(rest) = rest.strip_prefix('"') else { continue };
            let Some((key, value)) = rest.split_once("\"=") else {
                continue;
            };
            match kind {
                "S" => {
                    v.strings.insert(key.to_string(), value.to_string());
                }
                "D" => {
                    if let Ok(n) = u32::from_str_radix(value.trim(), 16) {
                        v.dwords.insert(key.to_string(), n);
                    }
                }
                _ => {} // B: (binary) and Z: (multi-line) entries aren't needed
            }
        }
        v
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        self.strings.get(key).map(String::as_str).filter(|s| !s.is_empty())
    }

    pub fn dword(&self, key: &str) -> Option<u32> {
        self.dwords.get(key).copied()
    }
}

/// Convert one session file. `Err` carries a warning for the preview.
pub fn parse_session(folder_path: Vec<String>, name: &str, text: &str) -> Result<ImportedConnection, String> {
    let ini = IniValues::parse(text);
    let protocol = ini.string("Protocol Name").unwrap_or("SSH2");
    let kind = match protocol {
        "SSH2" | "SSH1" => ConnectionKind::Ssh,
        "Telnet" | "Raw" => ConnectionKind::Telnet,
        "Serial" => ConnectionKind::Serial,
        "RDP" => ConnectionKind::Rdp,
        other => return Err(format!("Skipped '{name}': {other} sessions are not supported")),
    };

    let endpoint = if kind == ConnectionKind::Serial {
        let port = ini
            .string("Com Port")
            .ok_or_else(|| format!("Skipped '{name}': serial session has no COM port"))?;
        Endpoint::Serial(SerialSettings {
            port: port.to_string(),
            baud: ini.dword("Baud Rate").unwrap_or(9600),
            data_bits: ini.dword("Data Bits").map(|d| d as u8).unwrap_or(8),
            parity: match ini.dword("Parity").unwrap_or(0) {
                1 => Parity::Odd,
                2 => Parity::Even,
                3 => Parity::Mark,
                4 => Parity::Space,
                _ => Parity::None,
            },
            // SecureCRT: 0 = 1 stop bit, 1 = 1.5, 2 = 2.
            stop_bits: if ini.dword("Stop Bits") == Some(2) { 2 } else { 1 },
            flow: if ini.dword("CTS Flow Control") == Some(1) || ini.dword("DTR Flow Control") == Some(1) {
                FlowControl::Hardware
            } else if ini.dword("XON Flow Control") == Some(1) {
                FlowControl::Software
            } else {
                FlowControl::None
            },
        })
    } else {
        let host = ini
            .string("Hostname")
            .ok_or_else(|| format!("Skipped '{name}': no hostname"))?;
        let port_key = match protocol {
            "SSH2" => "[SSH2] Port",
            "SSH1" => "[SSH1] Port",
            "RDP" => "[RDP] Port",
            _ => "Port",
        };
        let port = ini
            .dword(port_key)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .or(kind.default_port())
            .unwrap_or(0);
        Endpoint::Network {
            host: host.to_string(),
            port,
        }
    };

    let jump_host = ini
        .string("Firewall Name")
        .filter(|f| !f.eq_ignore_ascii_case("None"))
        .map(|f| f.trim_start_matches("Session:").to_string());

    Ok(ImportedConnection {
        folder_path,
        name: name.to_string(),
        kind,
        endpoint,
        username: ini.string("Username").map(str::to_string),
        jump_host,
        notes: ini.string("Description").unwrap_or_default().replace('\u{1}', "\n"),
    })
}

/// Import a whole `Config/Sessions` directory.
pub fn import_dir(sessions_dir: &Path) -> io::Result<Import> {
    let mut import = Import {
        source: "SecureCRT".into(),
        ..Default::default()
    };
    walk(sessions_dir, &mut Vec::new(), &mut import)?;
    import.folders.sort();
    import
        .connections
        .sort_by(|a, b| (&a.folder_path, &a.name).cmp(&(&b.folder_path, &b.name)));
    Ok(import)
}

fn walk(dir: &Path, path: &mut Vec<String>, import: &mut Import) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            path.push(file_name);
            import.folders.push(path.clone());
            walk(&p, path, import)?;
            path.pop();
        } else if let Some(stem) = file_name.strip_suffix(".ini") {
            if stem == "__FolderData__" || (path.is_empty() && stem == "Default") {
                continue;
            }
            let bytes = fs::read(&p)?;
            // SecureCRT writes UTF-8 (newer) or a BOM-prefixed file.
            let text = String::from_utf8_lossy(&bytes);
            let text = text.trim_start_matches('\u{feff}');
            match parse_session(path.clone(), stem, text) {
                Ok(c) => import.connections.push(c),
                Err(w) => import.warnings.push(w),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SSH: &str = "S:\"Protocol Name\"=SSH2\r\nS:\"Hostname\"=10.1.1.1\r\nS:\"Username\"=admin\r\nD:\"[SSH2] Port\"=00000016\r\nS:\"Firewall Name\"=Session:bastion\r\n";

    #[test]
    fn ssh_session() {
        let c = parse_session(vec!["DC1".into()], "spine01", SSH).unwrap();
        assert_eq!(c.kind, ConnectionKind::Ssh);
        assert_eq!(
            c.endpoint,
            Endpoint::Network {
                host: "10.1.1.1".into(),
                port: 22
            }
        );
        assert_eq!(c.username.as_deref(), Some("admin"));
        assert_eq!(c.jump_host.as_deref(), Some("bastion"));
    }

    #[test]
    fn telnet_custom_port_and_serial() {
        let t = parse_session(
            vec![],
            "cons",
            "S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=cs1\nD:\"Port\"=000007d5\n",
        )
        .unwrap();
        assert_eq!(
            t.endpoint,
            Endpoint::Network {
                host: "cs1".into(),
                port: 2005
            }
        );

        let s = parse_session(
            vec![],
            "console",
            "S:\"Protocol Name\"=Serial\nS:\"Com Port\"=COM3\nD:\"Baud Rate\"=0001c200\nD:\"Parity\"=00000002\nD:\"CTS Flow Control\"=00000001\n",
        )
        .unwrap();
        let Endpoint::Serial(ser) = s.endpoint else {
            panic!("expected serial")
        };
        assert_eq!(
            (ser.port.as_str(), ser.baud, ser.parity, ser.flow),
            ("COM3", 115200, Parity::Even, FlowControl::Hardware)
        );
    }

    #[test]
    fn unsupported_protocol_is_a_warning() {
        let e = parse_session(vec![], "modem", "S:\"Protocol Name\"=TAPI\n").unwrap_err();
        assert!(e.contains("TAPI"));
    }

    #[test]
    fn imports_a_directory_tree() {
        let root = std::env::temp_dir().join(format!("linecard-scrt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("DC1/Spine")).unwrap();
        fs::write(root.join("Default.ini"), "S:\"Hostname\"=\n").unwrap();
        fs::write(root.join("DC1/__FolderData__.ini"), "").unwrap();
        fs::write(root.join("DC1/Spine/spine01.ini"), SSH).unwrap();
        fs::write(root.join("DC1/modem.ini"), "S:\"Protocol Name\"=TAPI\n").unwrap();
        fs::write(
            root.join("lab.ini"),
            "\u{feff}S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=lab\n",
        )
        .unwrap();

        let imp = import_dir(&root).unwrap();
        assert_eq!(
            imp.folders,
            vec![vec!["DC1".to_string()], vec!["DC1".into(), "Spine".into()]]
        );
        let names: Vec<_> = imp.connections.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["lab", "spine01"]);
        assert_eq!(imp.connections[1].folder_path, vec!["DC1", "Spine"]);
        assert_eq!(imp.warnings.len(), 1);
        assert_eq!(imp.summary(), "2 sessions in 2 folders, 1 warning");
        fs::remove_dir_all(&root).unwrap();
    }
}
