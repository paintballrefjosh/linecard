//! Session drivers.
//!
//! Every protocol implements [`Session`]: the UI sends keystrokes with
//! [`Session::write`], resizes with [`Session::resize`], and receives
//! [`SessionEvent`]s through the callback given at connect time. Tabs,
//! panes, logging and reconnect therefore work the same for every protocol.

use std::io;

pub use linecard_core_model::ConnectionKind;

pub mod telnet;

/// Something the session reports back to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connected,
    /// Bytes to render in the terminal.
    Data(Vec<u8>),
    /// Window title set by the remote side (OSC 0/2).
    Title(String),
    /// Session ended. `reason` is `None` for a clean close.
    Disconnected {
        reason: Option<String>,
    },
}

/// Callback receiving session events. Called from the driver's I/O thread.
pub type EventSink = Box<dyn FnMut(SessionEvent) + Send + 'static>;

/// A live connection.
pub trait Session: Send {
    fn kind(&self) -> ConnectionKind;
    /// Send user input (keystrokes, paste) to the remote side.
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// Terminal size changed (ignored by protocols that don't support it).
    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;
    fn close(&mut self) -> io::Result<()>;
}

/// What a driver needs to open a terminal session.
#[derive(Debug, Clone)]
pub struct TerminalTarget {
    pub host: String,
    pub port: u16,
    pub cols: u16,
    pub rows: u16,
    /// Reported to the server, e.g. "xterm-256color".
    pub term_type: String,
}

impl TerminalTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        TerminalTarget {
            host: host.into(),
            port,
            cols: 80,
            rows: 24,
            term_type: "xterm-256color".into(),
        }
    }
}

/// Opens a session of the right kind. Drivers not built yet return
/// `Unsupported` with a pointer to the roadmap.
pub fn connect_terminal(
    kind: ConnectionKind,
    target: &TerminalTarget,
    events: EventSink,
) -> io::Result<Box<dyn Session>> {
    match kind {
        ConnectionKind::Telnet => Ok(Box::new(telnet::TelnetSession::connect(target, events)?)),
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{other:?} driver not implemented yet (see docs/roadmap.md)"),
        )),
    }
}
