//! RDP on top of IronRDP.
//!
//! TLS with Network Level Authentication (CredSSP/NTLM), falling back to
//! plain TLS where the server allows it. The server's certificate is
//! checked like an SSH host key: the first time, and whenever it changes,
//! the user decides (and can save it). Screen updates arrive as changed
//! regions of an RGBA image; the session follows the tab's size through the
//! Display Control channel when the server supports it.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_cliprdr::{Client, Cliprdr};
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::{ClientConnector, ConnectionResult, ConnectorErrorKind, ConnectorResult, ServerName};
use ironrdp_core::{impl_as_any, WriteBuf};
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::MousePdu;
use ironrdp_pdu::rdp::capability_sets::{client_codecs_capabilities, MajorPlatformType};
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_rdpsnd::client::{Rdpsnd, RdpsndClientHandler};
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{fast_path, ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{single_sequence_step_read, split_tokio_framed, FramedWrite, NetworkClient};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::gfx::{scancode, Buttons, CursorShape, GfxEvent, GfxSession, GfxSink, MAX_CURSOR};
use crate::ssh::{JumpChain, SharedReplies};
use crate::{
    AuthPrompt, Credentials, HostKeyDecision, HostKeyPrompt, HostKeyStatus, PromptField, PromptReply, SshOptions,
    TerminalTarget,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const AUTH_ATTEMPTS: usize = 3;

/// Saved passwords, and the file of trusted server certificates.
#[derive(Clone, Debug)]
pub struct RdpOptions {
    pub credentials: Option<Arc<dyn Credentials>>,
    /// Lines of `host:port SHA256:<base64>`.
    pub known_certs: PathBuf,
    /// For logging in to SSH jump hosts, when the target has them.
    pub ssh: SshOptions,
}

enum Cmd {
    Input(Vec<FastPathInputEvent>),
    Resize(u16, u16),
    /// The local clipboard text, offered to the server.
    Clipboard(String),
    Close,
}

pub struct RdpSession {
    cmds: UnboundedSender<Cmd>,
    replies: UnboundedSender<PromptReply>,
    buttons: Buttons,
}

impl RdpSession {
    /// Start connecting in the background, asking for a `width`×`height`
    /// desktop. Each session runs on its own thread (IronRDP's futures
    /// aren't `Send`, so they can't share a multi-threaded runtime).
    pub fn connect(
        target: &TerminalTarget,
        width: u16,
        height: u16,
        opts: RdpOptions,
        events: GfxSink,
    ) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (reply_tx, reply_rx) = unbounded_channel();
        let target = target.clone();
        let events = Arc::new(Mutex::new(events));
        let thread = std::thread::Builder::new().name(format!("rdp {}", target.host));
        thread.spawn(move || {
            rt.block_on(async move {
                let emit: Arc<dyn Fn(GfxEvent) + Send + Sync> = Arc::new(move |e| {
                    if let Ok(mut s) = events.lock() {
                        s(e)
                    }
                });
                let (clip_tx, clip_rx) = unbounded_channel();
                let mut ui = Ui {
                    clip_tx,
                    clip_rx,
                    emit: emit.clone(),
                    replies: Arc::new(tokio::sync::Mutex::new(reply_rx)),
                    cmds: cmd_rx,
                };
                let reason = match run(&target, width, height, &opts, &mut ui).await {
                    Ok(()) => None,
                    Err(e) => Some(e.to_string()),
                };
                emit(GfxEvent::Disconnected { reason });
            })
        })?;
        Ok(RdpSession {
            cmds: cmd_tx,
            replies: reply_tx,
            buttons: 0,
        })
    }

    fn send(&self, c: Cmd) -> io::Result<()> {
        self.cmds
            .send(c)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }
}

fn mouse(flags: PointerFlags, x: u16, y: u16, wheel: i16) -> FastPathInputEvent {
    FastPathInputEvent::MouseEvent(MousePdu {
        flags,
        number_of_wheel_rotation_units: wheel,
        x_position: x,
        y_position: y,
    })
}

fn unicode(c: char, down: bool) -> Vec<FastPathInputEvent> {
    let flags = if down {
        KeyboardFlags::empty()
    } else {
        KeyboardFlags::RELEASE
    };
    let mut units = [0u16; 2];
    c.encode_utf16(&mut units)
        .iter()
        .map(|u| FastPathInputEvent::UnicodeKeyboardEvent(flags, *u))
        .collect()
}

impl GfxSession for RdpSession {
    fn pointer(&mut self, x: u16, y: u16, buttons: Buttons) -> io::Result<()> {
        let mut events = vec![mouse(PointerFlags::MOVE, x, y, 0)];
        for (bit, flag) in [
            (1, PointerFlags::LEFT_BUTTON),
            (2, PointerFlags::MIDDLE_BUTTON_OR_WHEEL),
            (4, PointerFlags::RIGHT_BUTTON),
        ] {
            let (was, now) = (self.buttons & bit != 0, buttons & bit != 0);
            if was != now {
                events.push(mouse(if now { flag | PointerFlags::DOWN } else { flag }, x, y, 0));
            }
        }
        self.buttons = buttons;
        self.send(Cmd::Input(events))
    }

    fn wheel(&mut self, x: u16, y: u16, dx: i16, dy: i16) -> io::Result<()> {
        let mut events = Vec::new();
        // One notch is 120 units; positive scrolls up (vertical) or right.
        for _ in 0..dy.unsigned_abs() {
            events.push(mouse(
                PointerFlags::VERTICAL_WHEEL,
                x,
                y,
                if dy > 0 { -120 } else { 120 },
            ));
        }
        for _ in 0..dx.unsigned_abs() {
            events.push(mouse(
                PointerFlags::HORIZONTAL_WHEEL,
                x,
                y,
                if dx > 0 { 120 } else { -120 },
            ));
        }
        self.send(Cmd::Input(events))
    }

    fn key(&mut self, code: &str, key: &str, down: bool) -> io::Result<()> {
        if let Some((sc, extended)) = scancode(code) {
            let mut flags = if down {
                KeyboardFlags::empty()
            } else {
                KeyboardFlags::RELEASE
            };
            if extended {
                flags |= KeyboardFlags::EXTENDED;
            }
            return self.send(Cmd::Input(vec![FastPathInputEvent::KeyboardEvent(flags, sc)]));
        }
        // Keys without a scancode here (other layouts): send the character.
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => self.send(Cmd::Input(unicode(c, down))),
            _ => Ok(()),
        }
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        let mut events = Vec::new();
        for c in text.chars() {
            if c == '\n' {
                events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1c));
                events.push(FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1c));
            } else if c != '\r' {
                events.extend(unicode(c, true));
                events.extend(unicode(c, false));
            }
        }
        self.send(Cmd::Input(events))
    }

    fn reply(&mut self, reply: PromptReply) -> io::Result<()> {
        self.replies
            .send(reply)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }

    fn clipboard(&mut self, text: &str) -> io::Result<()> {
        self.send(Cmd::Clipboard(text.to_string()))
    }

    fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.send(Cmd::Resize(width, height))
    }

    fn close(&mut self) -> io::Result<()> {
        let _ = self.replies.send(PromptReply::Cancel);
        let _ = self.cmds.send(Cmd::Close);
        Ok(())
    }
}

// ---- talking to the UI -------------------------------------------------------------

struct Ui {
    emit: Arc<dyn Fn(GfxEvent) + Send + Sync>,
    replies: SharedReplies,
    cmds: UnboundedReceiver<Cmd>,
    clip_tx: UnboundedSender<ClipEvent>,
    clip_rx: UnboundedReceiver<ClipEvent>,
}

fn canceled() -> io::Error {
    io::Error::other("canceled")
}

impl Ui {
    /// Resolves when the tab is closed.
    async fn closed(&mut self) {
        while let Some(c) = self.cmds.recv().await {
            if matches!(c, Cmd::Close) {
                return;
            }
        }
    }

    /// Wait for the answer to a prompt; closing the tab cancels.
    async fn answer(&mut self) -> io::Result<PromptReply> {
        loop {
            tokio::select! {
                r = async { self.replies.lock().await.recv().await } => return r.ok_or_else(canceled),
                c = self.cmds.recv() => if matches!(c, Some(Cmd::Close) | None) {
                    return Err(canceled());
                },
            }
        }
    }

    async fn ask(&mut self, prompt: AuthPrompt) -> io::Result<(Vec<String>, bool)> {
        let n = prompt.fields.len();
        (self.emit)(GfxEvent::Auth(prompt));
        loop {
            match self.answer().await? {
                PromptReply::Fields { mut values, remember } => {
                    values.resize(n, String::new());
                    return Ok((values, remember));
                }
                PromptReply::Cancel => return Err(canceled()),
                PromptReply::HostKey(_) => {}
            }
        }
    }
}

// ---- certificates --------------------------------------------------------------------

pub(crate) fn fingerprint(der: &[u8]) -> String {
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(der))
    )
}

pub(crate) enum Known {
    Match,
    Changed(usize),
    Unknown,
}

pub(crate) fn lookup(path: &std::path::Path, who: &str, fp: &str) -> Known {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Known::Unknown;
    };
    let mut changed = None;
    for (i, line) in text.lines().enumerate() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some(who) {
            if parts.next() == Some(fp) {
                return Known::Match;
            }
            changed.get_or_insert(i + 1);
        }
    }
    changed.map_or(Known::Unknown, Known::Changed)
}

pub(crate) fn remember_cert(path: &std::path::Path, who: &str, fp: &str) -> io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(who))
        .map(str::to_string)
        .collect();
    lines.push(format!("{who} {fp}"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

async fn check_certificate(der: &[u8], target: &TerminalTarget, opts: &RdpOptions, ui: &mut Ui) -> io::Result<()> {
    let who = format!("{}:{}", target.host, target.port);
    let fp = fingerprint(der);
    let status = match lookup(&opts.known_certs, &who, &fp) {
        Known::Match => return Ok(()),
        Known::Unknown => HostKeyStatus::Unknown,
        Known::Changed(line) => HostKeyStatus::Changed {
            known_hosts: opts.known_certs.clone(),
            line,
        },
    };
    (ui.emit)(GfxEvent::HostKey(HostKeyPrompt {
        host: target.host.clone(),
        port: target.port,
        algorithm: "TLS certificate".into(),
        fingerprint: fp.clone(),
        status,
    }));
    loop {
        match ui.answer().await? {
            PromptReply::HostKey(HostKeyDecision::AcceptOnce) => return Ok(()),
            PromptReply::HostKey(HostKeyDecision::AcceptAndSave) => {
                // Still connect if saving fails; it just asks again next time.
                let _ = remember_cert(&opts.known_certs, &who, &fp);
                return Ok(());
            }
            PromptReply::HostKey(HostKeyDecision::Reject) | PromptReply::Cancel => {
                return Err(io::Error::other("certificate not accepted"))
            }
            PromptReply::Fields { .. } => {}
        }
    }
}

// ---- connecting ------------------------------------------------------------------------

/// Kerberos would need to reach a KDC; NTLM (what nearly every RDP server
/// accepts) doesn't, so network requests are refused.
struct NoNetwork;

impl NetworkClient for NoNetwork {
    async fn send(&mut self, _request: &NetworkRequest) -> ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!(
            "Kerberos isn't supported; the server must allow NTLM"
        ))
    }
}

trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}
type Upgraded = ironrdp_tokio::TokioFramed<Box<dyn AsyncReadWrite + Unpin + Send + Sync>>;

/// "DOMAIN\user" or "user@domain" → (user, domain).
fn split_user(user: &str) -> (String, Option<String>) {
    if let Some((d, u)) = user.split_once('\\') {
        return (u.to_string(), Some(d.to_string()));
    }
    (user.to_string(), None)
}

fn connector_config(user: &str, password: &str, width: u16, height: u16) -> ironrdp_connector::Config {
    let (username, domain) = split_user(user);
    ironrdp_connector::Config {
        credentials: ironrdp_connector::Credentials::UsernamePassword {
            username,
            password: password.to_string(),
        },
        domain,
        enable_tls: true,
        enable_credssp: true,
        keyboard_type: ironrdp_pdu::gcc::KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: ironrdp_connector::DesktopSize { width, height },
        desktop_scale_factor: 0,
        monitor_layout: None,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        enable_standard_rdp_security: false,
        connection_type: ironrdp_pdu::gcc::ConnectionType::Lan,
        remote_application_mode: false,
        rail_support_level: ironrdp_pdu::rdp::capability_sets::RailSupportLevel::empty(),
        enable_audio_capture: false,
        support_dyn_vc_gfx_protocol: false,
        bitmap: Some(ironrdp_connector::BitmapConfig {
            color_depth: 32,
            lossy_compression: true,
            codecs: client_codecs_capabilities(&[]).unwrap_or_default(),
        }),
        client_build: 0,
        client_name: "Linecard".into(),
        client_dir: String::new(),
        platform: MajorPlatformType::UNSPECIFIED,
        hardware_id: None,
        license_cache: None,
        enable_server_pointer: true,
        autologon: false,
        enable_audio_playback: true,
        request_data: None,
        pointer_software_rendering: false,
        multitransport_flags: None,
        compression_type: Some(ironrdp_pdu::rdp::client_info::CompressionType::K64),
        performance_flags: PerformanceFlags::default(),
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    }
}

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Whether a failed connection was the credentials being refused.
fn refused(e: &ironrdp_connector::ConnectorError) -> bool {
    matches!(
        e.kind(),
        ConnectorErrorKind::Credssp(_) | ConnectorErrorKind::AccessDenied
    )
}

/// Who to log in as, gathered across attempts.
struct Login {
    user: Option<String>,
    password: Option<String>,
    remember: bool,
    /// Why the last attempt failed, shown in the next prompt.
    problem: String,
    /// A certificate the user accepted for this session only.
    accepted: Option<String>,
}

impl Login {
    fn slot(&self, target: &TerminalTarget) -> String {
        target.credential_key.clone().unwrap_or_else(|| {
            format!(
                "rdp:{}@{}:{}",
                self.user.as_deref().unwrap_or_default(),
                target.host,
                target.port
            )
        })
    }

    /// Ask for what's missing (after the server's certificate is trusted, so
    /// a password never goes to a server the user hasn't accepted).
    async fn complete(&mut self, target: &TerminalTarget, opts: &RdpOptions, ui: &mut Ui) -> io::Result<()> {
        if self.password.is_none() && self.user.is_some() {
            self.password = opts.credentials.as_ref().and_then(|c| c.password(&self.slot(target)));
        }
        if self.user.is_some() && self.password.is_some() {
            return Ok(());
        }
        let mut fields = Vec::new();
        if self.user.is_none() {
            fields.push(PromptField {
                label: "User name".into(),
                echo: true,
            });
        }
        fields.push(PromptField {
            label: "Password".into(),
            echo: false,
        });
        let (values, keep) = ui
            .ask(AuthPrompt {
                title: match &self.user {
                    Some(u) => format!("Password for {u} on {}", target.host),
                    None => format!("Log in to {}", target.host),
                },
                instructions: self.problem.clone(),
                fields,
                remember: opts.credentials.as_ref().map(|_| true),
            })
            .await?;
        let mut values = values.into_iter();
        if self.user.is_none() {
            self.user = values.next().filter(|u| !u.trim().is_empty());
        }
        self.password = values.next();
        self.remember = keep;
        Ok(())
    }
}

async fn attempt(
    target: &TerminalTarget,
    login: &mut Login,
    width: u16,
    height: u16,
    opts: &RdpOptions,
    chain: Option<&JumpChain>,
    ui: &mut Ui,
) -> io::Result<Result<(ConnectionResult, Upgraded), ironrdp_connector::ConnectorError>> {
    let (stream, client_addr): (Box<dyn AsyncReadWrite + Unpin + Send + Sync>, _) = match chain {
        Some(chain) => (
            Box::new(chain.connect(&target.host, target.port).await?),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        ),
        None => {
            let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((target.host.as_str(), target.port)))
                .await
                .map_err(|_| err(format!("timed out connecting to {}", target.host)))??;
            let _ = tcp.set_nodelay(true);
            let addr = tcp.local_addr()?;
            (Box::new(tcp), addr)
        }
    };
    let mut framed = ironrdp_tokio::TokioFramed::new(stream);
    let drdynvc = ironrdp_dvc::DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let clipboard = Cliprdr::<Client>::new(Box::new(ClipBackend {
        events: ui.clip_tx.clone(),
        emit: ui.emit.clone(),
    }));
    let user = login.user.clone().unwrap_or_default();
    let mut connector = ClientConnector::new(connector_config(&user, "", width, height), client_addr)
        .with_static_channel(drdynvc)
        .with_static_channel(clipboard)
        .with_static_channel(Rdpsnd::new(Box::new(Sound::new(ui.emit.clone()))));

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .map_err(|e| err(format!("RDP negotiation failed: {}", describe(&e))))?;
    let (stream, leftover) = framed.into_inner();
    let (tls, cert) = ironrdp_tls::upgrade(stream, &target.host)
        .await
        .map_err(|e| err(format!("the TLS handshake failed: {}", with_hint(&e.to_string()))))?;
    let der = x509_cert::der::Encode::to_der(&cert).map_err(err)?;
    let fp = fingerprint(&der);
    if login.accepted.as_deref() != Some(fp.as_str()) {
        check_certificate(&der, target, opts, ui).await?;
        login.accepted = Some(fp);
    }

    login.complete(target, opts, ui).await?;
    let fresh = connector_config(
        login.user.as_deref().unwrap_or_default(),
        login.password.as_deref().unwrap_or_default(),
        width,
        height,
    );
    connector.config.credentials = fresh.credentials;
    connector.config.domain = fresh.domain;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let erased: Box<dyn AsyncReadWrite + Unpin + Send + Sync> = Box::new(tls);
    let mut framed = ironrdp_tokio::TokioFramed::new_with_leftover(erased, leftover);
    let key = ironrdp_tls::extract_tls_server_public_key(&cert)
        .ok_or_else(|| err("the server's certificate has no public key"))?
        .to_owned();
    match ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        ServerName::new(target.host.clone()),
        key,
        None,
    )
    .await
    {
        Ok(ironrdp_connector::ConnectionOutcome::Connected(result)) => Ok(Ok((result, framed))),
        Ok(ironrdp_connector::ConnectionOutcome::Redirected(_)) => Err(err(
            "the server redirected to another host (a broker); Linecard doesn't follow redirects yet",
        )),
        Err(e) if refused(&e) => Ok(Err(e)),
        Err(e) => Err(err(format!("logging in failed: {}", describe(&e)))),
    }
}

async fn run(target: &TerminalTarget, width: u16, height: u16, opts: &RdpOptions, ui: &mut Ui) -> io::Result<()> {
    let mut login = Login {
        user: target.username.clone().filter(|u| !u.is_empty()),
        password: None,
        remember: false,
        problem: String::new(),
        accepted: None,
    };
    // Logged in once; each attempt opens a fresh channel through it.
    let chain = match &target.jump {
        Some(jump) => {
            let sink = crate::gfx::prompts_only(ui.emit.clone());
            let opening = JumpChain::open(jump, &opts.ssh, sink, ui.replies.clone());
            let chain = tokio::select! {
                r = opening => r?,
                _ = ui.closed() => return Err(canceled()),
            };
            Some(chain)
        }
        None => None,
    };
    for _ in 0..AUTH_ATTEMPTS {
        match attempt(target, &mut login, width, height, opts, chain.as_ref(), ui).await? {
            Ok((result, framed)) => {
                if login.remember {
                    if let (Some(c), Some(u), Some(p)) = (&opts.credentials, &login.user, &login.password) {
                        c.save_password(&login.slot(target), &format!("{u}@{} (RDP)", target.host), p);
                    }
                }
                return session(result, framed, &target.host, ui).await;
            }
            Err(e) if refused(&e) => {
                login.problem = "The server refused that user name or password.".into();
                login.password = None;
                // A saved password that stopped working is asked for again.
                login.remember = true;
            }
            Err(e) => return Err(err(describe(&e))),
        }
    }
    Err(err("the server refused the user name or password"))
}

fn describe(e: &ironrdp_connector::ConnectorError) -> String {
    match e.kind() {
        ConnectorErrorKind::Negotiation(f) => format!("the server refused the connection ({f})"),
        _ => with_hint(&e.to_string()),
    }
}

/// A dropped connection says little on its own; say what usually causes it.
fn with_hint(msg: &str) -> String {
    let m = msg.to_lowercase();
    if m.contains("10054") || m.contains("forcibly closed") || m.contains("connection reset") {
        format!(
            "{msg}. The server dropped the connection: older hosts (Windows 7, Server 2008 R2) may only offer TLS 1.0, \
             and accounts not allowed to use Remote Desktop are dropped the same way"
        )
    } else {
        msg.to_string()
    }
}

// ---- the session -------------------------------------------------------------------------

fn rgba_rect(image: &DecodedImage, r: &InclusiveRectangle) -> (u16, u16, u16, u16, Vec<u8>) {
    let (iw, ih) = (image.width(), image.height());
    let left = r.left.min(iw.saturating_sub(1));
    let top = r.top.min(ih.saturating_sub(1));
    let w = r.right.min(iw - 1).saturating_sub(left) + 1;
    let h = r.bottom.min(ih - 1).saturating_sub(top) + 1;
    let (stride, bpp, data) = (image.stride(), image.bytes_per_pixel(), image.data());
    let mut out = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
    for row in 0..usize::from(h) {
        let start = (usize::from(top) + row) * stride + usize::from(left) * bpp;
        for px in data[start..start + usize::from(w) * bpp].chunks_exact(bpp) {
            out.extend([px[0], px[1], px[2], 255]);
        }
    }
    (left, top, w, h, out)
}

/// A server pointer as the UI wants it. One too big for a browser cursor
/// (or malformed) falls back to the ordinary arrow.
fn pointer_shape(p: &ironrdp_graphics::pointer::DecodedPointer) -> CursorShape {
    let (w, h) = (p.width, p.height);
    let sane = w > 0
        && h > 0
        && w <= MAX_CURSOR
        && h <= MAX_CURSOR
        && p.bitmap_data.len() == usize::from(w) * usize::from(h) * 4;
    if !sane {
        return CursorShape::Default;
    }
    CursorShape::Image {
        width: w,
        height: h,
        hot_x: p.hotspot_x.min(w - 1),
        hot_y: p.hotspot_y.min(h - 1),
        rgba: p.bitmap_data.clone(),
    }
}

// ---- clipboard (text only) ---------------------------------------------------------

/// What the clipboard channel's callbacks ask the session loop to do; they
/// run inside the protocol code, which the loop is driving.
#[derive(Debug)]
enum ClipEvent {
    /// The server wants to know what we could paste.
    Announce,
    /// The server copied text: ask for it.
    RemoteText,
    /// The server wants our clipboard text.
    Wanted(ClipboardFormatId),
}

/// Sound from the server, as 16-bit PCM for the window to play. Only PCM is
/// offered, so there is nothing to decode (servers send PCM when the client
/// offers nothing else).
struct Sound {
    emit: Arc<dyn Fn(GfxEvent) + Send + Sync>,
    formats: Vec<AudioFormat>,
}

impl std::fmt::Debug for Sound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sound")
    }
}

impl Sound {
    fn new(emit: Arc<dyn Fn(GfxEvent) + Send + Sync>) -> Self {
        // Best first: CD quality stereo, then smaller.
        let formats = [(2u16, 44_100u32), (1, 44_100), (2, 22_050), (1, 22_050), (1, 11_025)]
            .into_iter()
            .map(|(n_channels, n_samples_per_sec)| AudioFormat {
                format: WaveFormat::PCM,
                n_channels,
                n_samples_per_sec,
                n_avg_bytes_per_sec: n_samples_per_sec * u32::from(n_channels) * 2,
                n_block_align: n_channels * 2,
                bits_per_sample: 16,
                data: None,
            })
            .collect();
        Sound { emit, formats }
    }
}

impl RdpsndClientHandler for Sound {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn choose_formats(&self, server_formats: &[AudioFormat]) -> Vec<AudioFormat> {
        let name = |f: &AudioFormat| {
            format!(
                "{:?} {}ch {}Hz {}bit",
                f.format, f.n_channels, f.n_samples_per_sec, f.bits_per_sample
            )
        };
        let list = |formats: &[AudioFormat]| formats.iter().map(name).collect::<Vec<_>>().join(", ");
        let chosen = ironrdp_rdpsnd::pdu::retain_offered(self.formats.clone(), server_formats);
        (self.emit)(GfxEvent::Log(format!(
            "sound: the server offers [{}]; Linecard accepts [{}]",
            list(server_formats),
            list(&chosen)
        )));
        chosen
    }

    fn wave(&mut self, format: &AudioFormat, _ts: u32, data: std::borrow::Cow<'_, [u8]>) {
        if format.format == WaveFormat::PCM && format.bits_per_sample == 16 && !data.is_empty() {
            (self.emit)(GfxEvent::Audio {
                channels: format.n_channels,
                rate: format.n_samples_per_sec,
                pcm: data.into_owned(),
            });
        }
    }

    fn set_volume(&mut self, _volume: VolumePdu) {}

    fn set_pitch(&mut self, _pitch: PitchPdu) {}

    fn close(&mut self) {}
}

struct ClipBackend {
    events: UnboundedSender<ClipEvent>,
    emit: Arc<dyn Fn(GfxEvent) + Send + Sync>,
}

impl std::fmt::Debug for ClipBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipBackend")
    }
}

impl_as_any!(ClipBackend);

impl CliprdrBackend for ClipBackend {
    fn temporary_directory(&self) -> &str {
        ".linecard-clip"
    }

    // Text only: no file transfer.
    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        let _ = self.events.send(ClipEvent::Announce);
    }

    fn on_request_format_list(&mut self) {
        let _ = self.events.send(ClipEvent::Announce);
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        if formats.iter().any(|f| f.id() == ClipboardFormatId::CF_UNICODETEXT) {
            let _ = self.events.send(ClipEvent::RemoteText);
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let _ = self.events.send(ClipEvent::Wanted(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        if let Ok(text) = response.to_unicode_string() {
            let text = text.trim_end_matches('\0').replace("\r\n", "\n");
            if !text.is_empty() {
                (self.emit)(GfxEvent::Clipboard(text));
            }
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}
    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}
    fn on_lock(&mut self, _: LockDataId) {}
    fn on_unlock(&mut self, _: LockDataId) {}
}

/// The bytes to send for a clipboard action; none while the channel is not
/// up yet (the text is offered when it is).
fn clip_frame(
    stage: &mut ActiveStage,
    act: impl FnOnce(&mut Cliprdr<Client>) -> ironrdp_pdu::PduResult<ironrdp_cliprdr::CliprdrSvcMessages<Client>>,
) -> io::Result<Vec<u8>> {
    let Some(clip) = stage.get_svc_processor_mut::<Cliprdr<Client>>() else {
        return Ok(Vec::new());
    };
    if !clip.is_ready() {
        return Ok(Vec::new());
    }
    let messages = act(clip).map_err(err)?;
    stage.process_svc_processor_messages(messages).map_err(err)
}

fn offer_text(stage: &mut ActiveStage, text: &Option<String>) -> io::Result<Vec<u8>> {
    if text.is_none() {
        return Ok(Vec::new());
    }
    clip_frame(stage, |c| {
        c.initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
    })
}

/// The server asked what we could paste. While the channel is starting up
/// this must be answered even with nothing to offer (an empty list), or it
/// never becomes ready.
fn announce(stage: &mut ActiveStage, text: &Option<String>) -> io::Result<Vec<u8>> {
    let Some(clip) = stage.get_svc_processor_mut::<Cliprdr<Client>>() else {
        return Ok(Vec::new());
    };
    if clip.is_ready() {
        return offer_text(stage, text);
    }
    let formats: Vec<ClipboardFormat> = text
        .iter()
        .map(|_| ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT))
        .collect();
    let messages = clip.initiate_copy(&formats).map_err(err)?;
    stage.process_svc_processor_messages(messages).map_err(err)
}

async fn session(result: ConnectionResult, framed: Upgraded, host: &str, ui: &mut Ui) -> io::Result<()> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    let activation_factory = result.activation_factory;
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    (ui.emit)(GfxEvent::Connected {
        width: size.width,
        height: size.height,
        name: host.to_string(),
    });

    let mut local_text: Option<String> = None;

    loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                let (action, payload) = frame?;
                stage.process(&mut image, action, &payload, None).map_err(err)?
            }
            cmd = ui.cmds.recv() => match cmd {
                Some(Cmd::Input(events)) => stage.process_fastpath_input(&mut image, &events).map_err(err)?,
                Some(Cmd::Resize(w, h)) => {
                    let (w, h) = MonitorLayoutEntry::adjust_display_size(u32::from(w), u32::from(h));
                    match stage.encode_resize(w, h, None, None) {
                        Some(frame) => vec![ActiveStageOutput::ResponseFrame(frame.map_err(err)?)],
                        None => Vec::new(), // the server can't resize; the view scales
                    }
                }
                Some(Cmd::Clipboard(text)) => {
                    local_text = Some(text);
                    vec![ActiveStageOutput::ResponseFrame(offer_text(&mut stage, &local_text)?)]
                }
                Some(Cmd::Close) | None => stage.graceful_shutdown().map_err(err)?,
            },
            Some(event) = ui.clip_rx.recv() => {
                let frame = match event {
                    ClipEvent::Announce => announce(&mut stage, &local_text)?,
                    ClipEvent::RemoteText => clip_frame(&mut stage, |c| c.initiate_paste(ClipboardFormatId::CF_UNICODETEXT))?,
                    ClipEvent::Wanted(format) => {
                        let text = local_text.clone().filter(|_| format == ClipboardFormatId::CF_UNICODETEXT);
                        clip_frame(&mut stage, |c| {
                            c.submit_format_data(match &text {
                                Some(t) => FormatDataResponse::new_unicode_string(&t.replace('\n', "\r\n")),
                                None => FormatDataResponse::new_error(),
                            })
                        })?
                    }
                };
                vec![ActiveStageOutput::ResponseFrame(frame)]
            }
        };
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if !frame.is_empty() {
                        writer.write_all(&frame).await?
                    }
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    let (x, y, width, height, rgba) = rgba_rect(&image, &region);
                    (ui.emit)(GfxEvent::Frame {
                        x,
                        y,
                        width,
                        height,
                        rgba,
                    });
                }
                ActiveStageOutput::PointerBitmap(p) => (ui.emit)(GfxEvent::Cursor(pointer_shape(&p))),
                ActiveStageOutput::PointerDefault => (ui.emit)(GfxEvent::Cursor(CursorShape::Default)),
                ActiveStageOutput::PointerHidden => (ui.emit)(GfxEvent::Cursor(CursorShape::Hidden)),
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation-Reactivation (e.g. after a resize): new size, same session.
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = single_sequence_step_read(&mut reader, &mut activation, &mut buf)
                            .await
                            .map_err(err)?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                            ..
                        } = activation.connection_activation_state()
                        {
                            image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            (ui.emit)(GfxEvent::Resized {
                                width: desktop_size.width,
                                height: desktop_size.height,
                            });
                            break;
                        }
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    return match reason.to_string().as_str() {
                        r if r.to_lowercase().contains("user requested") => Ok(()),
                        r => Err(err(r)),
                    };
                }
                _ => {} // pointer shapes, auto-detect, multitransport: not used
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_pointers_become_cursors() {
        use ironrdp_graphics::pointer::DecodedPointer;
        let ptr = |w: u16, h: u16, hot_x: u16, len: usize| DecodedPointer {
            width: w,
            height: h,
            hotspot_x: hot_x,
            hotspot_y: 1,
            bitmap_data: vec![7; len],
        };
        assert_eq!(
            pointer_shape(&ptr(2, 2, 9, 16)), // hotspot outside: pulled in
            CursorShape::Image {
                width: 2,
                height: 2,
                hot_x: 1,
                hot_y: 1,
                rgba: vec![7; 16]
            }
        );
        assert_eq!(pointer_shape(&ptr(2, 2, 0, 3)), CursorShape::Default, "wrong size");
        assert_eq!(
            pointer_shape(&ptr(200, 200, 0, 200 * 200 * 4)),
            CursorShape::Default,
            "too big for a browser cursor"
        );
    }

    #[test]
    fn users_and_domains() {
        assert_eq!(split_user("CORP\\alice"), ("alice".into(), Some("CORP".into())));
        assert_eq!(split_user("alice@corp.example"), ("alice@corp.example".into(), None));
    }

    #[test]
    fn certificates_are_remembered_and_changes_noticed() {
        let path = std::env::temp_dir().join(format!("linecard-rdp-certs-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(matches!(lookup(&path, "h:3389", "SHA256:a"), Known::Unknown));
        remember_cert(&path, "h:3389", "SHA256:a").unwrap();
        remember_cert(&path, "other:3389", "SHA256:z").unwrap();
        assert!(matches!(lookup(&path, "h:3389", "SHA256:a"), Known::Match));
        assert!(matches!(lookup(&path, "h:3389", "SHA256:b"), Known::Changed(1)));
        remember_cert(&path, "h:3389", "SHA256:b").unwrap();
        assert!(matches!(lookup(&path, "h:3389", "SHA256:b"), Known::Match));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            2,
            "replaced, not added"
        );
        let _ = std::fs::remove_file(&path);
    }

    // The clipboard channel against IronRDP's own server side, joined directly:
    // there is no RDP test server, but the two ends of this channel can talk.
    use ironrdp_cliprdr::Server;
    use ironrdp_svc::{SvcMessage, SvcProcessor};

    #[derive(Default)]
    struct Seen {
        remote_formats: usize,
        wanted: Option<ClipboardFormatId>,
        text: Option<String>,
    }

    struct ServerSide(Arc<Mutex<Seen>>);

    impl std::fmt::Debug for ServerSide {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ServerSide")
        }
    }

    impl_as_any!(ServerSide);

    impl CliprdrBackend for ServerSide {
        fn temporary_directory(&self) -> &str {
            ""
        }
        fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
            ClipboardGeneralCapabilityFlags::empty()
        }
        fn on_ready(&mut self) {}
        fn on_request_format_list(&mut self) {}
        fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}
        fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
            self.0.lock().unwrap().remote_formats = formats.len();
        }
        fn on_format_data_request(&mut self, r: FormatDataRequest) {
            self.0.lock().unwrap().wanted = Some(r.format);
        }
        fn on_format_data_response(&mut self, r: FormatDataResponse<'_>) {
            self.0.lock().unwrap().text = r.to_unicode_string().ok();
        }
        fn on_file_contents_request(&mut self, _: FileContentsRequest) {}
        fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}
        fn on_lock(&mut self, _: LockDataId) {}
        fn on_unlock(&mut self, _: LockDataId) {}
    }

    /// Carry messages back and forth until neither side has more to say.
    fn pump(
        client: &mut Cliprdr<Client>,
        server: &mut Cliprdr<Server>,
        from_client: Vec<SvcMessage>,
        from_server: Vec<SvcMessage>,
    ) {
        let (mut to_server, mut to_client) = (from_client, from_server);
        while !to_server.is_empty() || !to_client.is_empty() {
            let mut next_client = Vec::new();
            for m in std::mem::take(&mut to_server) {
                next_client.extend(server.process(&m.encode_unframed_pdu().unwrap()).unwrap());
            }
            let mut next_server = Vec::new();
            for m in std::mem::take(&mut to_client) {
                next_server.extend(client.process(&m.encode_unframed_pdu().unwrap()).unwrap());
            }
            to_client.extend(next_client);
            to_server.extend(next_server);
        }
    }

    type Rig = (
        Cliprdr<Client>,
        Cliprdr<Server>,
        Arc<Mutex<Seen>>,
        Arc<Mutex<Vec<GfxEvent>>>,
        UnboundedReceiver<ClipEvent>,
    );

    fn connected() -> Rig {
        let (tx, mut rx) = unbounded_channel();
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let log = emitted.clone();
        let emit: Arc<dyn Fn(GfxEvent) + Send + Sync> = Arc::new(move |e| log.lock().unwrap().push(e));
        let mut client = Cliprdr::<Client>::new(Box::new(ClipBackend { events: tx, emit }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut server = Cliprdr::<Server>::new(Box::new(ServerSide(seen.clone())));
        let from_server = server.start().unwrap();
        let from_client = client.start().unwrap();
        pump(&mut client, &mut server, from_client, from_server);
        // What the session loop does when asked to announce: answer with an empty list.
        assert!(
            matches!(rx.try_recv(), Ok(ClipEvent::Announce)),
            "asked for the format list"
        );
        let first = client.initiate_copy(&[]).unwrap();
        pump(&mut client, &mut server, messages(first), Vec::new());
        assert!(client.is_ready(), "the channel comes up");
        while rx.try_recv().is_ok() {}
        (client, server, seen, emitted, rx)
    }

    fn messages<R: ironrdp_cliprdr::Role>(m: ironrdp_cliprdr::CliprdrSvcMessages<R>) -> Vec<SvcMessage> {
        m.into()
    }

    #[test]
    fn text_copied_on_the_server_reaches_the_ui() {
        let (mut client, mut server, _seen, emitted, mut rx) = connected();
        let copy = server
            .initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
            .unwrap();
        pump(&mut client, &mut server, Vec::new(), messages(copy));
        // The backend asks the session loop to fetch it...
        let mut asked = false;
        while let Ok(e) = rx.try_recv() {
            asked |= matches!(e, ClipEvent::RemoteText);
        }
        assert!(asked);
        let paste = client.initiate_paste(ClipboardFormatId::CF_UNICODETEXT).unwrap();
        let reply = server
            .submit_format_data(FormatDataResponse::new_unicode_string("h\u{e9}llo\r\nthere"))
            .unwrap();
        // ...and the server answers the request that the paste sent.
        pump(&mut client, &mut server, messages(paste), Vec::new());
        pump(&mut client, &mut server, Vec::new(), messages(reply));
        assert_eq!(
            emitted.lock().unwrap().as_slice(),
            [GfxEvent::Clipboard("h\u{e9}llo\nthere".into())]
        );
    }

    #[test]
    fn local_text_is_offered_and_handed_over_when_asked_for() {
        let (mut client, mut server, seen, _emitted, mut rx) = connected();
        let copy = client
            .initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
            .unwrap();
        pump(&mut client, &mut server, messages(copy), Vec::new());
        assert_eq!(seen.lock().unwrap().remote_formats, 1);
        let paste = server.initiate_paste(ClipboardFormatId::CF_UNICODETEXT).unwrap();
        pump(&mut client, &mut server, Vec::new(), messages(paste));
        let mut wanted = None;
        while let Ok(e) = rx.try_recv() {
            if let ClipEvent::Wanted(f) = e {
                wanted = Some(f);
            }
        }
        assert_eq!(wanted, Some(ClipboardFormatId::CF_UNICODETEXT));
        let reply = client
            .submit_format_data(FormatDataResponse::new_unicode_string("show version\r\n"))
            .unwrap();
        pump(&mut client, &mut server, messages(reply), Vec::new());
        assert_eq!(seen.lock().unwrap().text.as_deref(), Some("show version\r\n"));
    }

    #[test]
    fn sound_is_offered_as_pcm_and_passed_on() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        let mut sound = Sound::new(Arc::new(move |e| sink.lock().unwrap().push(e)));
        let formats = sound.get_formats().to_vec();
        assert!(formats
            .iter()
            .all(|f| f.format == WaveFormat::PCM && f.bits_per_sample == 16));
        assert_eq!((formats[0].n_channels, formats[0].n_samples_per_sec), (2, 44_100));
        assert_eq!(formats[0].n_avg_bytes_per_sec, 44_100 * 4);

        sound.wave(&formats[1], 0, std::borrow::Cow::Borrowed(&[1, 0, 2, 0]));
        sound.wave(&formats[1], 0, std::borrow::Cow::Borrowed(&[]));
        let other = AudioFormat {
            bits_per_sample: 8,
            ..formats[1].clone()
        };
        sound.wave(&other, 0, std::borrow::Cow::Borrowed(&[9]));
        assert_eq!(
            heard.lock().unwrap().as_slice(),
            [GfxEvent::Audio {
                channels: 1,
                rate: 44_100,
                pcm: vec![1, 0, 2, 0]
            }],
            "only non-empty 16-bit PCM is played"
        );
    }
}
