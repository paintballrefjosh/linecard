//! SSH (RFC 4251–4254) on top of `russh`.
//!
//! The session runs as a task on the caller's tokio runtime. [`SshSession`]
//! itself is a pair of channel senders, so `write` / `resize` / `reply` /
//! `close` never block the UI thread. Anything that needs the user (host key
//! decisions, passwords, keyboard-interactive prompts) goes out as a
//! [`SessionEvent`] and the task waits for the matching [`PromptReply`].
//!
//! Authentication order follows OpenSSH: the agent (Windows OpenSSH agent,
//! then Pageant), unencrypted identity files, keyboard-interactive, password.

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{cipher, kex, mac, ChannelMsg, Disconnect, MethodKind, MethodSet, Preferred};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::{
    AuthPrompt, ConnectionKind, Credentials, EventSink, HostKeyDecision, HostKeyPrompt, HostKeyStatus, PromptField,
    PromptReply, Session, SessionEvent, SshOptions, TerminalTarget,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Interactive attempts before giving up, as OpenSSH does.
const AUTH_ATTEMPTS: usize = 3;

enum Cmd {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// A live SSH shell. Dropping it closes the connection.
pub struct SshSession {
    cmds: UnboundedSender<Cmd>,
    replies: UnboundedSender<PromptReply>,
}

impl SshSession {
    /// Start connecting in the background. Must be called inside a tokio
    /// runtime; progress arrives as events.
    pub fn connect(target: &TerminalTarget, opts: &SshOptions, events: EventSink) -> io::Result<Self> {
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| io::Error::other("SSH sessions must be started inside a tokio runtime"))?;
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (reply_tx, reply_rx) = unbounded_channel();
        let ui = Ui {
            events: Arc::new(Mutex::new(events)),
            replies: Arc::new(tokio::sync::Mutex::new(reply_rx)),
        };
        let target = target.clone();
        let opts = Arc::new(opts.clone());
        rt.spawn(async move {
            let reason = match run(target, opts, ui.clone(), cmd_rx).await {
                Ok(()) => None,
                Err(e) => Some(e.to_string()),
            };
            ui.emit(SessionEvent::Disconnected { reason });
        });
        Ok(SshSession {
            cmds: cmd_tx,
            replies: reply_tx,
        })
    }

    fn send(&self, cmd: Cmd) -> io::Result<()> {
        self.cmds
            .send(cmd)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }
}

impl Session for SshSession {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Ssh
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.send(Cmd::Data(data.to_vec()))
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.send(Cmd::Resize(cols, rows))
    }

    fn reply(&mut self, reply: PromptReply) -> io::Result<()> {
        self.replies
            .send(reply)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }

    fn close(&mut self) -> io::Result<()> {
        // Also release a task waiting on a prompt.
        let _ = self.replies.send(PromptReply::Cancel);
        let _ = self.cmds.send(Cmd::Close);
        Ok(())
    }
}

// ---- errors ------------------------------------------------------------------

#[derive(Debug)]
enum Failure {
    /// The user closed a prompt or the tab.
    Canceled,
    Msg(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Canceled => write!(f, "canceled"),
            Failure::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl From<russh::Error> for Failure {
    fn from(e: russh::Error) -> Self {
        Failure::Msg(match e {
            russh::Error::UnknownKey => "host key not accepted".into(),
            // Usually old gear; naming what it offers tells the user what to enable.
            russh::Error::NoCommonAlgo { kind, theirs, .. } => format!(
                "no common {} algorithm; the server offers: {}",
                format!("{kind:?}").to_lowercase(),
                theirs.join(", ")
            ),
            russh::Error::IO(io) => io.to_string(),
            other => other.to_string(),
        })
    }
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Failure::Msg(e.to_string())
    }
}

// ---- talking to the UI ---------------------------------------------------------

struct Answer {
    values: Vec<String>,
    remember: bool,
}

#[derive(Clone)]
struct Ui {
    events: Arc<Mutex<EventSink>>,
    replies: Arc<tokio::sync::Mutex<UnboundedReceiver<PromptReply>>>,
}

impl Ui {
    fn emit(&self, e: SessionEvent) {
        if let Ok(mut sink) = self.events.lock() {
            sink(e);
        }
    }

    /// Show server text (banners, notices) in the terminal.
    fn print(&self, text: &str) {
        let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
        self.emit(SessionEvent::Data(crlf.into_bytes()));
    }

    async fn next_reply(&self) -> PromptReply {
        self.replies.lock().await.recv().await.unwrap_or(PromptReply::Cancel)
    }

    async fn host_key(&self, prompt: HostKeyPrompt) -> HostKeyDecision {
        self.emit(SessionEvent::HostKey(prompt));
        loop {
            match self.next_reply().await {
                PromptReply::HostKey(d) => return d,
                PromptReply::Cancel => return HostKeyDecision::Reject,
                PromptReply::Fields { .. } => {} // stale answer to an earlier prompt
            }
        }
    }

    async fn ask(&self, prompt: AuthPrompt) -> Result<Answer, Failure> {
        let n = prompt.fields.len();
        self.emit(SessionEvent::Auth(prompt));
        loop {
            match self.next_reply().await {
                PromptReply::Fields { mut values, remember } => {
                    values.resize(n, String::new());
                    return Ok(Answer { values, remember });
                }
                PromptReply::Cancel => return Err(Failure::Canceled),
                PromptReply::HostKey(_) => {}
            }
        }
    }
}

// ---- host keys ---------------------------------------------------------------

enum Known {
    Match,
    Changed { path: PathBuf, line: usize },
    Unknown,
}

/// A match in any known_hosts file wins: after the user replaces a changed
/// key, Linecard's file matches even if `~/.ssh/known_hosts` still has the
/// old one.
fn lookup(host: &str, port: u16, key: &PublicKey, opts: &SshOptions) -> Known {
    let mut changed = None;
    for path in std::iter::once(&opts.known_hosts).chain(&opts.extra_known_hosts) {
        match keys::check_known_hosts_path(host, port, key, path) {
            Ok(true) => return Known::Match,
            Ok(false) => {}
            Err(keys::Error::KeyChanged { line }) => {
                changed.get_or_insert(Known::Changed {
                    path: path.clone(),
                    line,
                });
            }
            Err(_) => {} // missing or unreadable file
        }
    }
    changed.unwrap_or(Known::Unknown)
}

/// Remove one (1-based) line from a known_hosts file.
fn forget_line(path: &Path, line: usize) -> io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| i + 1 != line)
        .map(|(_, l)| l)
        .collect();
    std::fs::write(path, kept.join("\n") + "\n")
}

struct Client {
    host: String,
    port: u16,
    ui: Ui,
    opts: Arc<SshOptions>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn auth_banner(&mut self, banner: &str, _session: &mut client::Session) -> Result<(), Self::Error> {
        self.ui.print(banner);
        Ok(())
    }

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => PublicKey::new(cert.public_key().clone(), ""),
        };
        let status = match lookup(&self.host, self.port, &key, &self.opts) {
            Known::Match => return Ok(true),
            Known::Unknown => HostKeyStatus::Unknown,
            Known::Changed { path, line } => HostKeyStatus::Changed {
                known_hosts: path,
                line,
            },
        };
        let prompt = HostKeyPrompt {
            host: self.host.clone(),
            port: self.port,
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            status: status.clone(),
        };
        match self.ui.host_key(prompt).await {
            HostKeyDecision::Reject => Ok(false),
            HostKeyDecision::AcceptOnce => Ok(true),
            HostKeyDecision::AcceptAndSave => {
                let ours = &self.opts.known_hosts;
                let saved = (|| -> Result<(), keys::Error> {
                    if let HostKeyStatus::Changed { known_hosts, line } = &status {
                        if known_hosts == ours {
                            forget_line(ours, *line)?;
                        }
                    }
                    keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, ours)
                })();
                if let Err(e) = saved {
                    // Still connect; the user will just be asked again next time.
                    self.ui
                        .print(&format!("\x1b[33mCould not save the host key: {e}\x1b[0m\n"));
                }
                Ok(true)
            }
        }
    }
}

/// Modern algorithms first. The legacy ones that older switches, routers and
/// console servers still require come last, so they are only negotiated when
/// the server offers nothing better.
fn preferred() -> Preferred {
    let mut kex_order = Preferred::DEFAULT.kex.to_vec();
    kex_order.extend([kex::DH_GEX_SHA1, kex::DH_G14_SHA1]);
    let mut ciphers = Preferred::DEFAULT.cipher.to_vec();
    ciphers.extend([cipher::AES_256_CBC, cipher::AES_192_CBC, cipher::AES_128_CBC]);
    let mut macs = Preferred::DEFAULT.mac.to_vec();
    macs.extend([mac::HMAC_SHA1_ETM, mac::HMAC_SHA1]);
    Preferred {
        kex: Cow::Owned(kex_order),
        cipher: Cow::Owned(ciphers),
        mac: Cow::Owned(macs),
        ..Preferred::DEFAULT
    }
}

// ---- the session task ----------------------------------------------------------

/// Resolves when the tab is closed; input typed before the shell is up is dropped.
async fn until_closed(cmds: &mut UnboundedReceiver<Cmd>) {
    while let Some(cmd) = cmds.recv().await {
        if matches!(cmd, Cmd::Close) {
            return;
        }
    }
}

async fn run(
    target: TerminalTarget,
    opts: Arc<SshOptions>,
    ui: Ui,
    mut cmds: UnboundedReceiver<Cmd>,
) -> Result<(), Failure> {
    let config = Arc::new(client::Config {
        preferred: preferred(),
        keepalive_interval: target
            .keepalive_secs
            .filter(|s| *s > 0)
            .map(|s| Duration::from_secs(s.into())),
        keepalive_max: 3,
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    });
    let handler = Client {
        host: target.host.clone(),
        port: target.port,
        ui: ui.clone(),
        opts: opts.clone(),
    };

    let connecting = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect(config, (target.host.as_str(), target.port), handler),
    );
    let mut handle = tokio::select! {
        r = connecting => r.map_err(|_| Failure::Msg("timed out connecting".into()))??,
        _ = until_closed(&mut cmds) => return Err(Failure::Canceled),
    };

    tokio::select! {
        r = authenticate(&mut handle, &target, &opts, &ui) => r?,
        _ = until_closed(&mut cmds) => return Err(Failure::Canceled),
    }

    let channel = handle.channel_open_session().await?;
    channel
        .request_pty(
            false,
            &target.term_type,
            target.cols.into(),
            target.rows.into(),
            0,
            0,
            &[],
        )
        .await?;
    channel.request_shell(false).await?;
    ui.emit(SessionEvent::Connected);

    let (mut reader, writer) = channel.split();
    loop {
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    ui.emit(SessionEvent::Data(data.to_vec()));
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Data(d)) => writer.data(&d[..]).await?,
                Some(Cmd::Resize(cols, rows)) => writer.window_change(cols.into(), rows.into(), 0, 0).await?,
                Some(Cmd::Close) | None => {
                    let _ = writer.close().await;
                    break;
                }
            },
        }
    }
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(())
}

// ---- authentication ------------------------------------------------------------

/// Outcome of one method: in, or the methods the server will still take.
type Step = Result<(), MethodSet>;

fn step(r: client::AuthResult) -> Step {
    match r {
        client::AuthResult::Success => Ok(()),
        client::AuthResult::Failure { remaining_methods, .. } => Err(remaining_methods),
    }
}

async fn authenticate(
    h: &mut Handle<Client>,
    target: &TerminalTarget,
    opts: &SshOptions,
    ui: &Ui,
) -> Result<(), Failure> {
    let user = match target.username.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => u.to_string(),
        None => {
            let prompt = AuthPrompt {
                title: format!("Log in to {}", target.host),
                instructions: String::new(),
                fields: vec![PromptField {
                    label: "Username".into(),
                    echo: true,
                }],
                remember: None,
            };
            ui.ask(prompt).await?.values.remove(0)
        }
    };

    let mut methods = match step(h.authenticate_none(&user).await?) {
        Ok(()) => return Ok(()),
        Err(m) => m,
    };

    if methods.contains(&MethodKind::PublicKey) {
        if opts.use_agent && agent_auth(h, &user).await? {
            return Ok(());
        }
        for path in &opts.identity_files {
            // Missing and passphrase-protected keys are skipped for now.
            let Ok(key) = keys::load_secret_key(path, None) else {
                continue;
            };
            let hash = h.best_supported_rsa_hash().await?.flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            match step(h.authenticate_publickey(&user, key).await?) {
                Ok(()) => return Ok(()),
                Err(m) => methods = m,
            }
        }
    }

    let who = format!("{user}@{}", target.host);
    let mut pw = Passwords {
        creds: opts.credentials.as_deref(),
        key: target
            .credential_key
            .clone()
            .unwrap_or_else(|| format!("ssh:{user}@{}:{}", target.host, target.port)),
        label: who.clone(),
        saved: None,
        had_saved: false,
        typed: None,
    };
    pw.saved = pw.creds.and_then(|c| c.password(&pw.key));
    pw.had_saved = pw.saved.is_some();

    let mut denied = 0;
    while denied < AUTH_ATTEMPTS {
        let used_saved = pw.saved.is_some();
        let result = if methods.contains(&MethodKind::KeyboardInteractive) {
            keyboard_interactive(h, &user, &who, ui, &mut pw).await?
        } else if methods.contains(&MethodKind::Password) {
            let password = match pw.saved.take() {
                Some(p) => p,
                None => {
                    let prompt = AuthPrompt {
                        title: format!("Password for {who}"),
                        instructions: String::new(),
                        fields: vec![PromptField {
                            label: "Password".into(),
                            echo: false,
                        }],
                        remember: pw.offer(),
                    };
                    let a = ui.ask(prompt).await?;
                    let p = a.values.into_iter().next().unwrap_or_default();
                    pw.typed = Some((p.clone(), a.remember));
                    p
                }
            };
            step(h.authenticate_password(&user, password).await?)
        } else {
            break;
        };
        match result {
            Ok(()) => {
                pw.keep();
                return Ok(());
            }
            Err(m) => methods = m,
        }
        if used_saved && pw.saved.is_none() && pw.typed.is_none() {
            ui.print("\x1b[2mSaved password was not accepted\x1b[0m\n");
        } else {
            denied += 1;
            ui.print("\x1b[31mAccess denied\x1b[0m\n");
        }
        pw.typed = None;
    }
    Err(Failure::Msg(format!("authentication failed for {who}")))
}

/// The saved-password slot for one login: tried once, silently, before
/// asking; a password the user types is saved only if they ticked the box.
struct Passwords<'a> {
    creds: Option<&'a dyn Credentials>,
    key: String,
    label: String,
    /// Saved password not yet tried.
    saved: Option<String>,
    had_saved: bool,
    /// Last typed password and its "Save password" choice.
    typed: Option<(String, bool)>,
}

impl Passwords<'_> {
    /// Checkbox state to offer: ticked if a saved one exists (so a changed
    /// password updates it), absent when there's nowhere to save.
    fn offer(&self) -> Option<bool> {
        self.creds.map(|_| self.had_saved)
    }

    /// Login succeeded: save what was typed, if asked to.
    fn keep(&self) {
        if let (Some(creds), Some((p, true))) = (self.creds, &self.typed) {
            creds.save_password(&self.key, &self.label, p);
        }
    }
}

/// Network gear usually prompts this way ("Password:", sometimes a token).
/// A lone secret prompt is treated as the password: answered from the
/// saved one first, and offered for saving.
async fn keyboard_interactive(
    h: &mut Handle<Client>,
    user: &str,
    who: &str,
    ui: &Ui,
    pw: &mut Passwords<'_>,
) -> Result<Step, Failure> {
    let mut reply = h.authenticate_keyboard_interactive_start(user, None).await?;
    loop {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(Ok(())),
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, .. } => return Ok(Err(remaining_methods)),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let is_password = prompts.len() == 1 && !prompts[0].echo;
                let answers = if prompts.is_empty() {
                    Vec::new()
                } else if let (true, Some(saved)) = (is_password, pw.saved.take()) {
                    vec![saved]
                } else {
                    let prompt = AuthPrompt {
                        title: if name.trim().is_empty() {
                            format!("Log in to {who}")
                        } else {
                            name
                        },
                        instructions,
                        fields: prompts
                            .into_iter()
                            .map(|p| PromptField {
                                label: p.prompt.trim().trim_end_matches(':').to_string(),
                                echo: p.echo,
                            })
                            .collect(),
                        remember: if is_password { pw.offer() } else { None },
                    };
                    let a = ui.ask(prompt).await?;
                    if is_password {
                        pw.typed = a.values.first().map(|p| (p.clone(), a.remember));
                    }
                    a.values
                };
                reply = h.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
}

async fn with_agent<S>(h: &mut Handle<Client>, user: &str, mut agent: AgentClient<S>) -> Result<bool, Failure>
where
    S: AgentStream + Unpin + Send + 'static,
{
    let Ok(identities) = agent.request_identities().await else {
        return Ok(false);
    };
    for id in identities {
        let key = id.public_key().into_owned();
        let hash = if key.algorithm().is_rsa() {
            h.best_supported_rsa_hash().await?.flatten()
        } else {
            None
        };
        match h.authenticate_publickey_with(user, key, hash, &mut agent).await {
            Ok(r) if r.success() => return Ok(true),
            Ok(_) => {}
            Err(_) => return Ok(false), // agent went away or refused to sign
        }
    }
    Ok(false)
}

#[cfg(windows)]
async fn agent_auth(h: &mut Handle<Client>, user: &str) -> Result<bool, Failure> {
    if let Ok(agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        if with_agent(h, user, agent).await? {
            return Ok(true);
        }
    }
    match AgentClient::connect_pageant().await {
        Ok(agent) => with_agent(h, user, agent).await,
        Err(_) => Ok(false),
    }
}

#[cfg(not(windows))]
async fn agent_auth(h: &mut Handle<Client>, user: &str) -> Result<bool, Failure> {
    match AgentClient::connect_env().await {
        Ok(agent) => with_agent(h, user, agent).await,
        Err(_) => Ok(false),
    }
}
