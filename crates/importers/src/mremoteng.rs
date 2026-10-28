//! mRemoteNG import (`confCons.xml`).
//!
//! `<Node Type="Container">` elements are folders, `Type="Connection"` are
//! connections. A connection with `InheritUsername="true"` takes the user
//! name from its nearest container that sets one. Passwords are encrypted
//! with the user's master password and are not imported; a fully encrypted
//! file must be re-exported unencrypted from mRemoteNG first.

use std::path::Path;

use linecard_core_model::{ConnectionKind, Endpoint};

use crate::{Import, ImportedConnection};

pub fn parse(xml: &str) -> Result<Import, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("not a valid mRemoteNG file: {e}"))?;
    let root = doc.root_element();
    if root
        .attribute("FullFileEncryption")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return Err("this file is encrypted: in mRemoteNG, turn off full file encryption (or export the connections) and import again".into());
    }
    let mut import = Import {
        source: "mRemoteNG".into(),
        ..Default::default()
    };
    walk(root, &mut Vec::new(), &mut Vec::new(), &mut import);
    Ok(import)
}

fn walk<'a>(
    node: roxmltree::Node<'a, 'a>,
    path: &mut Vec<String>,
    users: &mut Vec<Option<&'a str>>,
    import: &mut Import,
) {
    for child in node.children().filter(|n| n.has_tag_name("Node")) {
        let name = child.attribute("Name").unwrap_or("").trim().to_string();
        match child.attribute("Type") {
            Some("Container") => {
                path.push(if name.is_empty() { "Folder".into() } else { name });
                import.folders.push(path.clone());
                users.push(child.attribute("Username").filter(|u| !u.is_empty()));
                walk(child, path, users, import);
                users.pop();
                path.pop();
            }
            Some("Connection") => match connection(child, &name, path, users) {
                Ok(c) => import.connections.push(c),
                Err(w) => import.warnings.push(w),
            },
            _ => {}
        }
    }
}

fn connection(
    n: roxmltree::Node,
    name: &str,
    path: &[String],
    users: &[Option<&str>],
) -> Result<ImportedConnection, String> {
    let protocol = n.attribute("Protocol").unwrap_or("SSH2");
    let kind = match protocol {
        "SSH1" | "SSH2" => ConnectionKind::Ssh,
        "Telnet" | "RAW" => ConnectionKind::Telnet,
        "RDP" => ConnectionKind::Rdp,
        "VNC" => ConnectionKind::Vnc,
        other => return Err(format!("Skipped '{name}': {other} connections are not supported")),
    };
    let host = n
        .attribute("Hostname")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("Skipped '{name}': no hostname"))?;
    let port = n
        .attribute("Port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .or(kind.default_port())
        .unwrap_or(0);
    let inherit = n
        .attribute("InheritUsername")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let username = if inherit {
        users.iter().rev().find_map(|u| *u).map(str::to_string)
    } else {
        n.attribute("Username").filter(|u| !u.is_empty()).map(str::to_string)
    };
    Ok(ImportedConnection {
        folder_path: path.to_vec(),
        name: if name.is_empty() {
            host.to_string()
        } else {
            name.to_string()
        },
        kind,
        endpoint: Endpoint::Network {
            host: host.to_string(),
            port,
        },
        username,
        jump_host: None,
        notes: n.attribute("Descr").unwrap_or("").to_string(),
    })
}

pub fn import_file(path: &Path) -> std::io::Result<Result<Import, String>> {
    Ok(parse(&std::fs::read_to_string(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<mrng:Connections xmlns:mrng="http://mremoteng.org" Name="Connections" Export="false" EncryptionEngine="AES" FullFileEncryption="false" ConfVersion="2.6">
  <Node Name="DC1" Type="Container" Username="netadmin">
    <Node Name="Spine" Type="Container">
      <Node Name="spine01" Type="Connection" Protocol="SSH2" Hostname="10.1.1.1" Port="22" Username="" InheritUsername="true" Descr="core" />
    </Node>
    <Node Name="jump" Type="Connection" Protocol="RDP" Hostname="jump.corp" Port="3389" Username="me" />
    <Node Name="web" Type="Connection" Protocol="HTTPS" Hostname="portal" Port="443" />
  </Node>
  <Node Name="cs1" Type="Connection" Protocol="Telnet" Hostname="cs1" Port="2005" />
</mrng:Connections>"#;

    #[test]
    fn containers_inheritance_and_protocols() {
        let imp = parse(XML).unwrap();
        assert_eq!(
            imp.folders,
            vec![vec!["DC1".to_string()], vec!["DC1".into(), "Spine".into()]]
        );
        let get = |n: &str| imp.connections.iter().find(|c| c.name == n).unwrap();
        let s = get("spine01");
        assert_eq!(s.folder_path, vec!["DC1", "Spine"]);
        assert_eq!(s.username.as_deref(), Some("netadmin"), "inherited from the container");
        assert_eq!(s.notes, "core");
        assert_eq!(get("jump").kind, ConnectionKind::Rdp);
        assert_eq!(
            get("cs1").endpoint,
            Endpoint::Network {
                host: "cs1".into(),
                port: 2005
            }
        );
        assert_eq!(imp.warnings.len(), 1, "HTTPS skipped");
    }

    #[test]
    fn encrypted_files_explain_what_to_do() {
        let e = parse(r#"<Connections FullFileEncryption="true">gibberish</Connections>"#).unwrap_err();
        assert!(e.contains("encrypted"));
        assert!(parse("not xml").is_err());
    }
}
