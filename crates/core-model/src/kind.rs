#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The protocol a connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "lowercase"))]
pub enum ConnectionKind {
    Ssh,
    Telnet,
    Serial,
    Rdp,
    Vnc,
}

impl ConnectionKind {
    pub const ALL: [ConnectionKind; 5] = [
        ConnectionKind::Ssh,
        ConnectionKind::Telnet,
        ConnectionKind::Serial,
        ConnectionKind::Rdp,
        ConnectionKind::Vnc,
    ];

    /// Default TCP port, or `None` for serial.
    pub fn default_port(self) -> Option<u16> {
        match self {
            ConnectionKind::Ssh => Some(22),
            ConnectionKind::Telnet => Some(23),
            ConnectionKind::Rdp => Some(3389),
            ConnectionKind::Vnc => Some(5900),
            ConnectionKind::Serial => None,
        }
    }

    /// Lucide icon name used by the UI.
    pub fn icon(self) -> &'static str {
        match self {
            ConnectionKind::Ssh | ConnectionKind::Telnet => "terminal-square",
            ConnectionKind::Serial => "cable",
            ConnectionKind::Rdp => "monitor",
            ConnectionKind::Vnc => "screen-share",
        }
    }

    /// Whether the session is a character terminal (vs. a graphical desktop).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ConnectionKind::Ssh | ConnectionKind::Telnet | ConnectionKind::Serial
        )
    }

    /// Whether traffic is encrypted on the wire.
    pub fn is_encrypted(self) -> bool {
        matches!(self, ConnectionKind::Ssh | ConnectionKind::Rdp)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ssh" | "ssh2" | "ssh1" => Some(ConnectionKind::Ssh),
            "telnet" => Some(ConnectionKind::Telnet),
            "serial" => Some(ConnectionKind::Serial),
            "rdp" => Some(ConnectionKind::Rdp),
            "vnc" => Some(ConnectionKind::Vnc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "lowercase"))]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
    Mark,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(rename_all = "lowercase"))]
pub enum FlowControl {
    #[default]
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SerialSettings {
    /// e.g. "COM3" or "/dev/ttyUSB0"
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: FlowControl,
}

impl SerialSettings {
    /// 9600 8N1, no flow control: the default for most network gear consoles.
    pub fn console(port: impl Into<String>) -> Self {
        SerialSettings {
            port: port.into(),
            baud: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow: FlowControl::None,
        }
    }
}

/// Where a connection goes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "type", rename_all = "lowercase")
)]
pub enum Endpoint {
    Network { host: String, port: u16 },
    Serial(SerialSettings),
}

impl Endpoint {
    pub fn network(kind: ConnectionKind, host: impl Into<String>) -> Self {
        Endpoint::Network {
            host: host.into(),
            port: kind.default_port().unwrap_or(0),
        }
    }

    /// Short human label, e.g. `10.0.0.1:2222` or `COM3 9600`.
    pub fn label(&self) -> String {
        match self {
            Endpoint::Network { host, port } => format!("{host}:{port}"),
            Endpoint::Serial(s) => format!("{} {}", s.port, s.baud),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_parse() {
        assert_eq!(ConnectionKind::Ssh.default_port(), Some(22));
        assert_eq!(ConnectionKind::Serial.default_port(), None);
        assert_eq!(ConnectionKind::parse("SSH2"), Some(ConnectionKind::Ssh));
        assert_eq!(ConnectionKind::parse("bogus"), None);
        assert!(!ConnectionKind::Telnet.is_encrypted());
        assert!(ConnectionKind::Serial.is_terminal());
    }

    #[test]
    fn endpoint_labels() {
        assert_eq!(Endpoint::network(ConnectionKind::Ssh, "sw1").label(), "sw1:22");
        assert_eq!(Endpoint::Serial(SerialSettings::console("COM3")).label(), "COM3 9600");
    }
}
