//! Session drivers.
//!
//! Every protocol implements [`Session`]: the UI sends keystrokes with
//! [`Session::write`], resizes with [`Session::resize`], answers prompts
//! with [`Session::reply`], and receives [`SessionEvent`]s through the
//! callback given at connect time. Tabs, panes, logging and reconnect
//! therefore work the same for every protocol.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

pub use forward::ForwardState;
pub use linecard_core_model::{ConnectionKind, ForwardKind, PortForward, SerialSettings};

pub mod forward;
pub mod serial;
pub mod ssh;
pub mod telnet;
pub mod xmodem;

/// Something the session reports back to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connected,
    /// Bytes to render in the terminal.
    Data(Vec<u8>),
    /// Window title set by the remote side (OSC 0/2).
    Title(String),
    /// SSH port forwards and whether each one is working.
    Forwards(Vec<ForwardState>),
    /// The server's host key needs the user's decision. Answer with
    /// [`PromptReply::HostKey`].
    HostKey(HostKeyPrompt),
    /// The server asks for credentials (password, keyboard-interactive,
    /// username). Answer with [`PromptReply::Fields`].
    Auth(AuthPrompt),
    /// Progress of a file transfer (XMODEM/YMODEM on serial lines).
    Transfer(TransferStatus),
    /// Session ended. `reason` is `None` for a clean close.
    Disconnected {
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    /// e.g. "ssh-ed25519"
    pub algorithm: String,
    /// e.g. "SHA256:ohD8VZEXGWo6Ez8GSEJQ9WpafgLFsOfLOtGGQCQo6Og"
    pub fingerprint: String,
    pub status: HostKeyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// First connection to this host.
    Unknown,
    /// A different key is recorded for this host: possibly an attack, or the
    /// device was replaced or re-keyed.
    Changed { known_hosts: PathBuf, line: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Trust the key and record it in Linecard's known_hosts.
    AcceptAndSave,
    /// Trust it for this session only.
    AcceptOnce,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPrompt {
    /// e.g. "Password for admin@10.0.0.1"
    pub title: String,
    /// Server-supplied text (keyboard-interactive), may be empty.
    pub instructions: String,
    pub fields: Vec<PromptField>,
    /// Offer "Save password" with this initial state (`None` = don't offer,
    /// e.g. for usernames and one-time codes).
    pub remember: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptField {
    pub label: String,
    /// false = secret (password); true = shown as typed (username, OTP).
    pub echo: bool,
}

/// The user's answer to a [`SessionEvent::HostKey`] or [`SessionEvent::Auth`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptReply {
    HostKey(HostKeyDecision),
    /// One value per [`PromptField`], in order; `remember` is the "Save
    /// password" checkbox (ignored when the prompt didn't offer it).
    Fields {
        values: Vec<String>,
        remember: bool,
    },
    /// The user closed the prompt: abandon the connection.
    Cancel,
}

/// Start a file transfer on a session (serial lines only for now).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferRequest {
    /// Send this file; the device must be waiting (e.g. ROMMON `xmodem`).
    Send { path: PathBuf, protocol: xmodem::Protocol },
    /// Receive into this file.
    Receive { path: PathBuf, protocol: xmodem::Protocol },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferStatus {
    /// File name shown to the user.
    pub file: String,
    pub sending: bool,
    pub done: u64,
    /// Known when sending, or for YMODEM.
    pub total: Option<u64>,
    /// `None` while running; then success or the reason it stopped.
    pub result: Option<Result<(), String>>,
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
    /// Answer a host-key or credential prompt.
    fn reply(&mut self, _reply: PromptReply) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this session has no prompts",
        ))
    }
    /// Start an XMODEM/YMODEM transfer; progress arrives as
    /// [`SessionEvent::Transfer`].
    fn start_transfer(&mut self, _req: TransferRequest) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file transfer is only available on serial sessions",
        ))
    }
    fn cancel_transfer(&mut self) {}
    /// Send a BREAK (serial consoles: password recovery, ROMMON).
    fn send_break(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "BREAK is only available on serial sessions",
        ))
    }
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
    /// Login name; SSH asks for one when `None`.
    pub username: Option<String>,
    /// SSH keepalive interval (`None` or 0 = off).
    pub keepalive_secs: Option<u32>,
    /// Saved-password slot to use (a shared credential's id). Default:
    /// one per `user@host:port`.
    pub credential_key: Option<String>,
    /// SSH jump host (ProxyJump) to tunnel through; may itself have one.
    pub jump: Option<Box<TerminalTarget>>,
    /// SSH port forwards to start once logged in.
    pub forwards: Vec<PortForward>,
}

impl TerminalTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        TerminalTarget {
            host: host.into(),
            port,
            cols: 80,
            rows: 24,
            term_type: "xterm-256color".into(),
            username: None,
            keepalive_secs: None,
            credential_key: None,
            jump: None,
            forwards: Vec::new(),
        }
    }
}

/// Saved passwords, as the app stores them (the OS keychain). Drivers only
/// see this interface.
pub trait Credentials: Send + Sync + fmt::Debug {
    fn password(&self, key: &str) -> Option<String>;
    /// `label` is what Settings shows, e.g. "admin@10.0.0.1".
    fn save_password(&self, key: &str, label: &str, password: &str);
}

/// App-wide SSH settings (where keys and known hosts live).
#[derive(Debug, Clone, Default)]
pub struct SshOptions {
    /// Saved passwords; `None` = never save or reuse them.
    pub credentials: Option<Arc<dyn Credentials>>,
    /// Linecard's own known_hosts; accepted keys are written here.
    pub known_hosts: PathBuf,
    /// Also trusted, never written (e.g. `~/.ssh/known_hosts`).
    pub extra_known_hosts: Vec<PathBuf>,
    /// Private keys tried before passwords (unencrypted ones only for now).
    pub identity_files: Vec<PathBuf>,
    /// Try keys from the Windows OpenSSH agent / Pageant / `SSH_AUTH_SOCK`.
    pub use_agent: bool,
}

/// Opens a session of the right kind. Drivers not built yet return
/// `Unsupported` with a pointer to the roadmap.
///
/// SSH sessions run on the current tokio runtime, so call this from inside
/// one (a `spawn_blocking` closure counts). They return at once and report
/// progress through `events`; Telnet connects before returning.
pub fn connect_terminal(
    kind: ConnectionKind,
    target: &TerminalTarget,
    ssh: &SshOptions,
    events: EventSink,
) -> io::Result<Box<dyn Session>> {
    match kind {
        ConnectionKind::Ssh => Ok(Box::new(ssh::SshSession::connect(target, ssh, events)?)),
        ConnectionKind::Telnet => Ok(Box::new(telnet::TelnetSession::connect(target, events)?)),
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{other:?} driver not implemented yet (see docs/roadmap.md)"),
        )),
    }
}

/// Open a serial console. Connects before returning.
pub fn connect_serial(settings: &SerialSettings, events: EventSink) -> io::Result<Box<dyn Session>> {
    Ok(Box::new(serial::SerialSession::connect(settings, events)?))
}
