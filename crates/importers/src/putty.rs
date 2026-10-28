//! PuTTY session import.
//!
//! PuTTY keeps each saved session under
//! `HKCU\Software\SimonTatham\PuTTY\Sessions\<url-encoded name>`. Parsing is
//! separate from the registry so it can be tested anywhere. Session names
//! containing `/` or `\` become folders (a common convention, also used by
//! SuperPuTTY and KiTTY).

use std::collections::HashMap;

use linecard_core_model::{ConnectionKind, Endpoint, FlowControl, Parity, SerialSettings};

use crate::{Import, ImportedConnection};

#[derive(Debug, Clone, PartialEq)]
pub enum RegValue {
    Str(String),
    Dword(u32),
}

/// PuTTY's registry key names escape spaces and punctuation as %XX.
pub fn decode_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_session(raw_name: &str, values: &HashMap<String, RegValue>) -> Result<ImportedConnection, String> {
    let full = decode_name(raw_name);
    let mut parts: Vec<String> = full
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let name = parts.pop().unwrap_or_else(|| full.clone());
    let s = |k: &str| match values.get(k) {
        Some(RegValue::Str(v)) if !v.is_empty() => Some(v.clone()),
        _ => None,
    };
    let d = |k: &str| match values.get(k) {
        Some(RegValue::Dword(v)) => Some(*v),
        _ => None,
    };

    let protocol = s("Protocol").unwrap_or_else(|| "ssh".into());
    let kind = match protocol.as_str() {
        "ssh" => ConnectionKind::Ssh,
        "telnet" | "raw" => ConnectionKind::Telnet,
        "serial" => ConnectionKind::Serial,
        other => return Err(format!("Skipped '{full}': {other} sessions are not supported")),
    };
    let mut username = s("UserName");
    let endpoint = if kind == ConnectionKind::Serial {
        let port = s("SerialLine").ok_or_else(|| format!("Skipped '{full}': serial session has no line"))?;
        Endpoint::Serial(SerialSettings {
            port,
            baud: d("SerialSpeed").unwrap_or(9600),
            data_bits: d("SerialDataBits").map(|b| b as u8).unwrap_or(8),
            parity: match d("SerialParity").unwrap_or(0) {
                1 => Parity::Odd,
                2 => Parity::Even,
                3 => Parity::Mark,
                4 => Parity::Space,
                _ => Parity::None,
            },
            // Stored in half-bits: 2 = 1, 3 = 1.5, 4 = 2.
            stop_bits: if d("SerialStopHalfbits") == Some(4) { 2 } else { 1 },
            flow: match d("SerialFlowControl").unwrap_or(1) {
                0 => FlowControl::None,
                2 | 3 => FlowControl::Hardware,
                _ => FlowControl::Software,
            },
        })
    } else {
        let raw_host = s("HostName").ok_or_else(|| format!("Skipped '{full}': no host name"))?;
        // "admin@sw1" is allowed in PuTTY's host field.
        let host = match raw_host.rsplit_once('@') {
            Some((user, host)) => {
                username = username.or_else(|| Some(user.to_string()));
                host.to_string()
            }
            None => raw_host,
        };
        let port = d("PortNumber")
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .or(kind.default_port())
            .unwrap_or(0);
        Endpoint::Network { host, port }
    };

    Ok(ImportedConnection {
        folder_path: parts,
        name,
        kind,
        endpoint,
        username,
        jump_host: None,
        notes: String::new(),
    })
}

/// Build an import from (raw name, values) pairs, as read from the registry.
pub fn from_sessions(sessions: Vec<(String, HashMap<String, RegValue>)>) -> Import {
    let mut import = Import {
        source: "PuTTY".into(),
        ..Default::default()
    };
    for (raw, values) in sessions {
        if decode_name(&raw) == "Default Settings" {
            continue;
        }
        match parse_session(&raw, &values) {
            Ok(c) => {
                for depth in 1..=c.folder_path.len() {
                    let p = c.folder_path[..depth].to_vec();
                    if !import.folders.contains(&p) {
                        import.folders.push(p);
                    }
                }
                import.connections.push(c);
            }
            Err(w) => import.warnings.push(w),
        }
    }
    import.folders.sort();
    import
        .connections
        .sort_by(|a, b| (&a.folder_path, &a.name).cmp(&(&b.folder_path, &b.name)));
    import
}

/// Read the current user's PuTTY sessions from the registry.
#[cfg(windows)]
pub fn import_registry() -> std::io::Result<Import> {
    use winreg::enums::{RegType, HKEY_CURRENT_USER};
    use winreg::RegKey;

    let root = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\SimonTatham\PuTTY\Sessions") {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(from_sessions(vec![])),
        Err(e) => return Err(e),
    };
    let mut sessions = Vec::new();
    for name in root.enum_keys().flatten() {
        let Ok(key) = root.open_subkey(&name) else { continue };
        let mut values = HashMap::new();
        for (vname, v) in key.enum_values().flatten() {
            let parsed = match v.vtype {
                RegType::REG_DWORD if v.bytes.len() >= 4 => Some(RegValue::Dword(u32::from_le_bytes([
                    v.bytes[0], v.bytes[1], v.bytes[2], v.bytes[3],
                ]))),
                RegType::REG_SZ | RegType::REG_EXPAND_SZ => Some(RegValue::Str(v.to_string())),
                _ => None,
            };
            if let Some(p) = parsed {
                values.insert(vname, p);
            }
        }
        sessions.push((name, values));
    }
    Ok(from_sessions(sessions))
}

#[cfg(not(windows))]
pub fn import_registry() -> std::io::Result<Import> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "PuTTY sessions live in the Windows registry",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, RegValue)]) -> HashMap<String, RegValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn names_decode_and_split_into_folders() {
        assert_eq!(decode_name("DC1%2FSpine%20A%2Fspine01"), "DC1/Spine A/spine01");
        assert_eq!(decode_name("100%"), "100%");
        let c = parse_session(
            "DC1%2Fspine01",
            &vals(&[
                ("Protocol", RegValue::Str("ssh".into())),
                ("HostName", RegValue::Str("admin@10.1.1.1".into())),
                ("PortNumber", RegValue::Dword(2222)),
            ]),
        )
        .unwrap();
        assert_eq!(
            (c.folder_path.clone(), c.name.as_str()),
            (vec!["DC1".to_string()], "spine01")
        );
        assert_eq!(c.username.as_deref(), Some("admin"), "user@ in the host field");
        assert_eq!(
            c.endpoint,
            Endpoint::Network {
                host: "10.1.1.1".into(),
                port: 2222
            }
        );
    }

    #[test]
    fn serial_and_unsupported() {
        let c = parse_session(
            "console",
            &vals(&[
                ("Protocol", RegValue::Str("serial".into())),
                ("SerialLine", RegValue::Str("COM4".into())),
                ("SerialSpeed", RegValue::Dword(115200)),
                ("SerialStopHalfbits", RegValue::Dword(4)),
                ("SerialFlowControl", RegValue::Dword(0)),
            ]),
        )
        .unwrap();
        let Endpoint::Serial(s) = c.endpoint else {
            panic!("serial")
        };
        assert_eq!(
            (s.port.as_str(), s.baud, s.stop_bits, s.flow),
            ("COM4", 115200, 2, FlowControl::None)
        );
        assert!(parse_session("x", &vals(&[("Protocol", RegValue::Str("rlogin".into()))])).is_err());
    }

    #[test]
    fn default_settings_skipped_and_folders_collected() {
        let ssh = |h: &str| {
            vals(&[
                ("Protocol", RegValue::Str("ssh".into())),
                ("HostName", RegValue::Str(h.into())),
            ])
        };
        let imp = from_sessions(vec![
            ("Default%20Settings".into(), ssh("x")),
            ("DC1%2FSpine%2Fs1".into(), ssh("s1")),
            ("lab".into(), ssh("lab")),
        ]);
        assert_eq!(imp.connections.len(), 2);
        assert_eq!(
            imp.folders,
            vec![vec!["DC1".to_string()], vec!["DC1".into(), "Spine".into()]]
        );
    }
}
