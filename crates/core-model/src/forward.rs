//! SSH port forwarding, set per connection.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "lowercase"))]
pub enum ForwardKind {
    /// `-L`: a port on this computer reaches `host:port` from the server.
    #[default]
    Local,
    /// `-R`: a port on the server reaches `host:port` from this computer.
    Remote,
    /// `-D`: a SOCKS proxy on this computer; each connection goes wherever
    /// the client asks, from the server.
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "camelCase"))]
pub struct PortForward {
    pub kind: ForwardKind,
    /// Where to listen (this computer for Local/Dynamic, the server for
    /// Remote). Empty = loopback only.
    #[cfg_attr(feature = "serde", serde(default))]
    pub bind_address: String,
    pub bind_port: u16,
    /// Destination; unused for Dynamic.
    #[cfg_attr(feature = "serde", serde(default))]
    pub host: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub port: u16,
}

impl PortForward {
    /// Listening address, with loopback for an empty one.
    pub fn bind(&self) -> &str {
        match self.bind_address.trim() {
            "" if self.kind == ForwardKind::Remote => "localhost",
            "" => "127.0.0.1",
            a => a,
        }
    }

    /// "L 127.0.0.1:8080 → 10.0.0.5:80", the way OpenSSH users read them.
    pub fn label(&self) -> String {
        let bind = format!("{}:{}", self.bind(), self.bind_port);
        match self.kind {
            ForwardKind::Local => format!("L {bind} → {}:{}", self.host, self.port),
            ForwardKind::Remote => format!("R {bind} → {}:{}", self.host, self.port),
            ForwardKind::Dynamic => format!("D {bind} (SOCKS)"),
        }
    }

    /// Why this forward can't work, if it can't.
    pub fn problem(&self) -> Option<String> {
        if self.bind_port == 0 && self.kind != ForwardKind::Remote {
            return Some("needs a port to listen on".into());
        }
        if self.kind != ForwardKind::Dynamic && (self.host.trim().is_empty() || self.port == 0) {
            return Some("needs a destination host and port".into());
        }
        None
    }
}
