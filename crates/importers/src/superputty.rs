//! SuperPuTTY import (`Sessions.xml`, usually in Documents\SuperPuTTY).
//!
//! Each `<SessionData>` has a `SessionId` like `DC1/Spine/spine01`: every
//! segment but the last is a folder. Serial sessions keep the COM port in
//! `Host` and the baud rate in `Port`.

use std::path::Path;

use linecard_core_model::{ConnectionKind, Endpoint, SerialSettings};

use crate::{Import, ImportedConnection};

pub fn parse(xml: &str) -> Result<Import, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("not a valid SuperPuTTY sessions file: {e}"))?;
    let mut import = Import {
        source: "SuperPuTTY".into(),
        ..Default::default()
    };
    for s in doc.descendants().filter(|n| n.has_tag_name("SessionData")) {
        let id = s.attribute("SessionId").unwrap_or("");
        let mut path: Vec<String> = id
            .split('/')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        let last = path.pop();
        let name = s
            .attribute("SessionName")
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string)
            .or(last)
            .unwrap_or_else(|| "session".into());
        match connection(s, &name, &path) {
            Ok(c) => {
                for depth in 1..=path.len() {
                    let p = path[..depth].to_vec();
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
    Ok(import)
}

fn connection(s: roxmltree::Node, name: &str, path: &[String]) -> Result<ImportedConnection, String> {
    let proto = s.attribute("Proto").unwrap_or("SSH");
    let kind = match proto {
        "SSH" => ConnectionKind::Ssh,
        "Telnet" | "Raw" => ConnectionKind::Telnet,
        "Serial" => ConnectionKind::Serial,
        other => return Err(format!("Skipped '{name}': {other} sessions are not supported")),
    };
    let host = s
        .attribute("Host")
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("Skipped '{name}': no host"))?;
    let port: Option<u32> = s
        .attribute("Port")
        .and_then(|p| p.trim().parse().ok())
        .filter(|p| *p != 0);
    let endpoint = if kind == ConnectionKind::Serial {
        let mut ser = SerialSettings::console(host);
        if let Some(baud) = port {
            ser.baud = baud;
        }
        Endpoint::Serial(ser)
    } else {
        Endpoint::Network {
            host: host.to_string(),
            port: port
                .and_then(|p| u16::try_from(p).ok())
                .or(kind.default_port())
                .unwrap_or(0),
        }
    };
    Ok(ImportedConnection {
        folder_path: path.to_vec(),
        name: name.to_string(),
        kind,
        endpoint,
        username: s.attribute("Username").filter(|u| !u.is_empty()).map(str::to_string),
        jump_host: None,
        notes: String::new(),
    })
}

pub fn import_file(path: &Path) -> std::io::Result<Result<Import, String>> {
    Ok(parse(&std::fs::read_to_string(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ArrayOfSessionData xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <SessionData SessionId="DC1/Spine/spine01" SessionName="spine01" ImageKey="computer" Host="10.1.1.1" Port="22" Proto="SSH" PuttySession="Default Settings" Username="admin" ExtraArgs="" />
  <SessionData SessionId="DC1/cs1-p5" SessionName="cs1 port 5" Host="10.1.99.5" Port="2005" Proto="Telnet" Username="" />
  <SessionData SessionId="Lab/console" SessionName="console" Host="COM3" Port="115200" Proto="Serial" />
  <SessionData SessionId="old/box" SessionName="box" Host="box" Port="513" Proto="Rlogin" />
</ArrayOfSessionData>"#;

    #[test]
    fn session_ids_become_folders() {
        let imp = parse(XML).unwrap();
        assert_eq!(
            imp.folders,
            vec![
                vec!["DC1".to_string()],
                vec!["DC1".into(), "Spine".into()],
                vec!["Lab".into()]
            ]
        );
        let get = |n: &str| imp.connections.iter().find(|c| c.name == n).unwrap();
        let s = get("spine01");
        assert_eq!(s.folder_path, vec!["DC1", "Spine"]);
        assert_eq!(s.username.as_deref(), Some("admin"));
        assert_eq!(
            get("cs1 port 5").endpoint,
            Endpoint::Network {
                host: "10.1.99.5".into(),
                port: 2005
            }
        );
        let Endpoint::Serial(ser) = &get("console").endpoint else {
            panic!("serial")
        };
        assert_eq!((ser.port.as_str(), ser.baud), ("COM3", 115200));
        assert_eq!(imp.warnings.len(), 1, "rlogin skipped");
    }
}
