//! VNC (RFB 3.3–3.8, RFC 6143).
//!
//! Security: None, VNC authentication (DES challenge) and Apple Remote
//! Desktop (macOS Screen Sharing: username and password over a
//! Diffie-Hellman exchange). Encodings: ZRLE, CopyRect and Raw, plus the
//! DesktopSize and Cursor pseudo-encodings. The client asks
//! for 32-bit true color laid out R, G, B, X in memory, keeps its own copy
//! of the screen (CopyRect and ZRLE need it) and reports changed
//! rectangles as RGBA.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use des::cipher::generic_array::GenericArray;
use des::cipher::{BlockEncrypt, KeyInit};
use flate2::{Decompress, FlushDecompress};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::gfx::{keysym, Buttons, CursorShape, GfxEvent, GfxSession, GfxSink, MAX_CURSOR};
use crate::ssh::{JumpChain, SharedReplies};
use crate::{AuthPrompt, Credentials, PromptField, PromptReply, SshOptions, TerminalTarget};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

enum Cmd {
    Pointer(u16, u16, Buttons),
    Wheel(u16, u16, i16, i16),
    Key(u32, bool),
    Text(String),
    Clipboard(String),
    Close,
}

/// ClientCutText: the RFB clipboard is Latin-1, so other characters become `?`.
pub fn client_cut_text(text: &str) -> Vec<u8> {
    let bytes: Vec<u8> = text
        .chars()
        .take(1 << 20)
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    let mut out = vec![6, 0, 0, 0];
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
    out
}

pub struct VncSession {
    cmds: UnboundedSender<Cmd>,
    replies: UnboundedSender<PromptReply>,
    /// Buttons last held, so wheel clicks can be added to them.
    buttons: Buttons,
}

impl VncSession {
    /// Start connecting in the background (inside a tokio runtime).
    /// `credentials` supplies and saves the VNC password.
    pub fn connect(
        target: &TerminalTarget,
        credentials: Option<Arc<dyn Credentials>>,
        ssh: SshOptions,
        events: GfxSink,
    ) -> io::Result<Self> {
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| io::Error::other("VNC sessions must be started inside a tokio runtime"))?;
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (reply_tx, reply_rx) = unbounded_channel();
        let target = target.clone();
        let events = Arc::new(Mutex::new(events));
        let replies: SharedReplies = Arc::new(tokio::sync::Mutex::new(reply_rx));
        rt.spawn(async move {
            let emit: Arc<dyn Fn(GfxEvent) + Send + Sync> = Arc::new(move |e| {
                if let Ok(mut s) = events.lock() {
                    s(e)
                }
            });
            let reason = match run(&target, credentials, &ssh, &emit, cmd_rx, replies).await {
                Ok(()) => None,
                Err(e) => Some(e.to_string()),
            };
            emit(GfxEvent::Disconnected { reason });
        });
        Ok(VncSession {
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

impl GfxSession for VncSession {
    fn pointer(&mut self, x: u16, y: u16, buttons: Buttons) -> io::Result<()> {
        self.buttons = buttons;
        // RFB: bit 0 left, 1 middle, 2 right; the same as ours.
        self.send(Cmd::Pointer(x, y, buttons))
    }

    fn wheel(&mut self, x: u16, y: u16, dx: i16, dy: i16) -> io::Result<()> {
        self.send(Cmd::Wheel(x, y, dx, dy))
    }

    fn key(&mut self, code: &str, key: &str, down: bool) -> io::Result<()> {
        match keysym(code, key) {
            Some(k) => self.send(Cmd::Key(k, down)),
            None => Ok(()),
        }
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        self.send(Cmd::Text(text.to_string()))
    }

    fn clipboard(&mut self, text: &str) -> io::Result<()> {
        self.send(Cmd::Clipboard(text.to_string()))
    }

    fn reply(&mut self, reply: PromptReply) -> io::Result<()> {
        self.replies
            .send(reply)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }

    fn resize(&mut self, _width: u16, _height: u16) -> io::Result<()> {
        Ok(()) // the server decides the size; the view scales
    }

    fn close(&mut self) -> io::Result<()> {
        let _ = self.replies.send(PromptReply::Cancel);
        let _ = self.cmds.send(Cmd::Close);
        Ok(())
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Resolves when the tab is closed; other commands are dropped until the
/// session is up.
async fn until_closed(cmds: &mut UnboundedReceiver<Cmd>) {
    while let Some(cmd) = cmds.recv().await {
        if matches!(cmd, Cmd::Close) {
            return;
        }
    }
}

/// Security type 30: Apple Remote Desktop.
const ARD: u8 = 30;

/// Apple Remote Desktop login: Diffie-Hellman with the server's `generator`
/// and `prime`, then username and password (64 bytes each, padded with
/// random bytes) encrypted with AES-128-ECB under the MD5 of the shared
/// secret. Returns the 128 encrypted bytes and our public key, both sent to
/// the server. `private` and `padding` are random.
pub fn ard_response(
    username: &str,
    password: &str,
    generator: u16,
    prime: &[u8],
    server_public: &[u8],
    private: &[u8],
    padding: &[u8; 128],
) -> (Vec<u8>, Vec<u8>) {
    use aes::cipher::{BlockEncrypt as _, KeyInit as _};
    use md5::{Digest, Md5};
    use num_bigint::BigUint;

    let p = BigUint::from_bytes_be(prime);
    let a = BigUint::from_bytes_be(private);
    let width = prime.len();
    let be = |n: &BigUint| {
        let raw = n.to_bytes_be();
        let mut out = vec![0u8; width.saturating_sub(raw.len())];
        out.extend(raw);
        out
    };
    let public = BigUint::from(generator).modpow(&a, &p);
    let shared = BigUint::from_bytes_be(server_public).modpow(&a, &p);
    let key = Md5::digest(be(&shared));

    let mut creds = *padding;
    for (field, text) in [(0usize, username), (64, password)] {
        let bytes = text.as_bytes();
        let n = bytes.len().min(63);
        creds[field..field + n].copy_from_slice(&bytes[..n]);
        creds[field + n] = 0;
    }
    let cipher = aes::Aes128::new(aes::cipher::generic_array::GenericArray::from_slice(&key));
    for block in creds.chunks_exact_mut(16) {
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(block));
    }
    (creds.to_vec(), be(&public))
}

/// The Cursor pseudo-encoding: `px` is `w*h` pixels laid out R, G, B, X and
/// `mask` one bit per pixel (rows padded to whole bytes), set where the
/// cursor is drawn. A cursor with no pixels hides the pointer.
fn cursor_shape(hot_x: u16, hot_y: u16, w: u16, h: u16, px: &[u8], mask: &[u8]) -> CursorShape {
    if w == 0 || h == 0 {
        return CursorShape::Hidden;
    }
    let row = usize::from(w).div_ceil(8);
    if w > MAX_CURSOR
        || h > MAX_CURSOR
        || px.len() != usize::from(w) * usize::from(h) * 4
        || mask.len() != row * usize::from(h)
    {
        return CursorShape::Default;
    }
    let mut rgba = Vec::with_capacity(px.len());
    for y in 0..usize::from(h) {
        for x in 0..usize::from(w) {
            let i = (y * usize::from(w) + x) * 4;
            let on = mask[y * row + x / 8] & (0x80 >> (x % 8)) != 0;
            rgba.extend([px[i], px[i + 1], px[i + 2], if on { 255 } else { 0 }]);
        }
    }
    CursorShape::Image {
        width: w,
        height: h,
        hot_x: hot_x.min(w - 1),
        hot_y: hot_y.min(h - 1),
        rgba,
    }
}

fn fail(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// VNC authentication: DES over the challenge, keyed with the password's
/// first 8 bytes with each byte's bits reversed (a quirk of the original).
pub fn vnc_auth_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new(GenericArray::from_slice(&key));
    let mut out = *challenge;
    for block in out.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

async fn run(
    target: &TerminalTarget,
    credentials: Option<Arc<dyn Credentials>>,
    ssh: &SshOptions,
    emit: &Arc<dyn Fn(GfxEvent) + Send + Sync>,
    mut cmds: UnboundedReceiver<Cmd>,
    replies: SharedReplies,
) -> io::Result<()> {
    // `_chain` keeps the jump-host tunnel alive for the session.
    let (stream, _chain): (Box<dyn Io>, Option<JumpChain>) = match &target.jump {
        Some(jump) => {
            let opening = async {
                let chain = JumpChain::open(jump, ssh, crate::gfx::prompts_only(emit.clone()), replies.clone()).await?;
                let tunnel = chain.connect(&target.host, target.port).await?;
                Ok::<_, io::Error>((Box::new(tunnel) as Box<dyn Io>, Some(chain)))
            };
            tokio::select! {
                r = opening => r?,
                _ = until_closed(&mut cmds) => return Err(fail("canceled")),
            }
        }
        None => {
            let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((target.host.as_str(), target.port)))
                .await
                .map_err(|_| fail(format!("timed out connecting to {}", target.host)))??;
            let _ = tcp.set_nodelay(true);
            (Box::new(tcp), None)
        }
    };
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);

    // Version.
    let mut ver = [0u8; 12];
    r.read_exact(&mut ver).await?;
    let text = String::from_utf8_lossy(&ver);
    if !text.starts_with("RFB ") {
        return Err(fail("not a VNC server"));
    }
    let minor: u32 = text[8..11].parse().unwrap_or(3);
    let minor = match minor {
        m if m >= 8 => 8,
        7 => 7,
        _ => 3, // includes 3.5 and Apple's 3.889
    };
    w.write_all(format!("RFB 003.00{minor}\n").as_bytes()).await?;

    // Security type.
    let chosen = if minor == 3 {
        r.read_u32().await?
    } else {
        let n = r.read_u8().await?;
        if n == 0 {
            return Err(fail(reason(&mut r).await));
        }
        let mut types = vec![0u8; n.into()];
        r.read_exact(&mut types).await?;
        let pick = if types.contains(&1) {
            1
        } else if types.contains(&ARD) && target.username.as_deref().is_some_and(|u| !u.is_empty()) {
            ARD // a login name is set: use the Mac account rather than a screen-sharing password
        } else if types.contains(&2) {
            2
        } else if types.contains(&ARD) {
            ARD
        } else {
            return Err(fail(format!(
                "the server wants a security type Linecard doesn't support yet ({types:?}); allow VNC password authentication on it"
            )));
        };
        w.write_u8(pick).await?;
        pick.into()
    };

    let slot = target
        .credential_key
        .clone()
        .unwrap_or_else(|| format!("vnc:{}:{}", target.host, target.port));
    let mut asked: Option<(String, bool)> = None;
    match chosen {
        0 => return Err(fail(reason(&mut r).await)),
        1 => {}
        2 => {
            let mut challenge = [0u8; 16];
            r.read_exact(&mut challenge).await?;
            let saved = credentials.as_ref().and_then(|c| c.password(&slot));
            let password = match saved {
                Some(p) => p,
                None => {
                    emit(GfxEvent::Auth(AuthPrompt {
                        title: format!("VNC password for {}", target.host),
                        instructions: String::new(),
                        fields: vec![PromptField {
                            label: "Password".into(),
                            echo: false,
                        }],
                        remember: credentials.as_ref().map(|_| true),
                    }));
                    loop {
                        tokio::select! {
                            reply = async { replies.lock().await.recv().await } => match reply {
                                Some(PromptReply::Fields { values, remember }) => {
                                    let p = values.into_iter().next().unwrap_or_default();
                                    asked = Some((p.clone(), remember));
                                    break p;
                                }
                                Some(PromptReply::HostKey(_)) => continue,
                                Some(PromptReply::Cancel) | None => return Err(fail("canceled")),
                            },
                            cmd = cmds.recv() => if matches!(cmd, Some(Cmd::Close) | None) {
                                return Err(fail("canceled"));
                            },
                        }
                    }
                }
            };
            w.write_all(&vnc_auth_response(&password, &challenge)).await?;
        }
        c if c == u32::from(ARD) => {
            let generator = r.read_u16().await?;
            let key_len = usize::from(r.read_u16().await?);
            if !(8..=1024).contains(&key_len) {
                return Err(fail("the server sent an unusable Apple Remote Desktop key"));
            }
            let mut prime = vec![0u8; key_len];
            r.read_exact(&mut prime).await?;
            let mut server_public = vec![0u8; key_len];
            r.read_exact(&mut server_public).await?;

            let saved = credentials.as_ref().and_then(|c| c.password(&slot));
            let (user, password) = match (target.username.clone().filter(|u| !u.is_empty()), saved) {
                (Some(u), Some(p)) => (u, p),
                (known_user, _) => {
                    let mut fields = Vec::new();
                    if known_user.is_none() {
                        fields.push(PromptField {
                            label: "Username".into(),
                            echo: true,
                        });
                    }
                    fields.push(PromptField {
                        label: "Password".into(),
                        echo: false,
                    });
                    emit(GfxEvent::Auth(AuthPrompt {
                        title: format!("Mac login for {}", target.host),
                        instructions: "Screen Sharing asks for a user account on that Mac.".into(),
                        fields,
                        remember: credentials.as_ref().map(|_| true),
                    }));
                    loop {
                        tokio::select! {
                            reply = async { replies.lock().await.recv().await } => match reply {
                                Some(PromptReply::Fields { values, remember }) => {
                                    let mut values = values.into_iter();
                                    let u = match &known_user {
                                        Some(u) => u.clone(),
                                        None => values.next().unwrap_or_default(),
                                    };
                                    let p = values.next().unwrap_or_default();
                                    asked = Some((p.clone(), remember));
                                    break (u, p);
                                }
                                Some(PromptReply::HostKey(_)) => continue,
                                Some(PromptReply::Cancel) | None => return Err(fail("canceled")),
                            },
                            cmd = cmds.recv() => if matches!(cmd, Some(Cmd::Close) | None) {
                                return Err(fail("canceled"));
                            },
                        }
                    }
                }
            };
            let mut private = vec![0u8; key_len];
            let mut padding = [0u8; 128];
            getrandom::getrandom(&mut private).map_err(|e| fail(e.to_string()))?;
            getrandom::getrandom(&mut padding).map_err(|e| fail(e.to_string()))?;
            let (sealed, public) =
                ard_response(&user, &password, generator, &prime, &server_public, &private, &padding);
            w.write_all(&sealed).await?;
            w.write_all(&public).await?;
        }
        other => return Err(fail(format!("unsupported security type {other}"))),
    }
    // SecurityResult (3.3/3.7 send none for "None").
    if (chosen == 2 || chosen == u32::from(ARD) || minor == 8) && r.read_u32().await? != 0 {
        let why = if minor == 8 {
            reason(&mut r).await
        } else {
            String::new()
        };
        return Err(fail(if chosen == 2 {
            "wrong VNC password".to_string()
        } else if chosen == u32::from(ARD) {
            "the Mac refused that username and password".to_string()
        } else if why.is_empty() {
            "the server refused the connection".to_string()
        } else {
            why
        }));
    }
    if let (Some((password, true)), Some(c)) = (&asked, &credentials) {
        c.save_password(&slot, &format!("VNC {}", target.host), password);
    }

    // Init.
    w.write_u8(1).await?; // shared: don't disconnect other viewers
    let mut width = r.read_u16().await?;
    let mut height = r.read_u16().await?;
    let mut pf = [0u8; 16];
    r.read_exact(&mut pf).await?;
    let name_len = r.read_u32().await?;
    let mut name = vec![0u8; name_len.min(1 << 16) as usize];
    r.read_exact(&mut name).await?;
    let name = String::from_utf8_lossy(&name).into_owned();

    // 32 bpp, depth 24, little endian, true color, R G B at shifts 0/8/16:
    // in memory R, G, B, X.
    let mut setup = vec![0u8, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0];
    let encodings: [i32; 5] = [16, 1, 0, -223, -239]; // ZRLE, CopyRect, Raw, DesktopSize, Cursor
    setup.extend([2u8, 0]);
    setup.extend((encodings.len() as u16).to_be_bytes());
    for e in encodings {
        setup.extend(e.to_be_bytes());
    }
    setup.extend(update_request(false, width, height));
    w.write_all(&setup).await?;
    emit(GfxEvent::Connected {
        width,
        height,
        name: name.clone(),
    });

    let mut screen = vec![0u8; usize::from(width) * usize::from(height) * 4];
    let mut zlib = Decompress::new(true);
    let mut buttons: Buttons = 0;

    loop {
        tokio::select! {
            msg = r.read_u8() => match msg? {
                0 => {
                    let _pad = r.read_u8().await?;
                    let n = r.read_u16().await?;
                    for _ in 0..n {
                        let (x, y, rw, rh) = (r.read_u16().await?, r.read_u16().await?, r.read_u16().await?, r.read_u16().await?);
                        let enc = r.read_i32().await?;
                        match enc {
                            -223 => {
                                width = rw;
                                height = rh;
                                screen = vec![0u8; usize::from(width) * usize::from(height) * 4];
                                emit(GfxEvent::Resized { width, height });
                                continue;
                            }
                            -239 => {
                                // Hotspot is (x, y); the shape is rw×rh pixels, then a bitmask.
                                let mut px = vec![0u8; usize::from(rw) * usize::from(rh) * 4];
                                r.read_exact(&mut px).await?;
                                let mut mask = vec![0u8; usize::from(rw).div_ceil(8) * usize::from(rh)];
                                r.read_exact(&mut mask).await?;
                                emit(GfxEvent::Cursor(cursor_shape(x, y, rw, rh, &px, &mask)));
                                continue;
                            }
                            0 => {
                                let mut px = vec![0u8; usize::from(rw) * usize::from(rh) * 4];
                                r.read_exact(&mut px).await?;
                                for p in px.as_chunks_mut::<4>().0 {
                                    p[3] = 255;
                                }
                                blit(&mut screen, width, x, y, rw, rh, &px);
                            }
                            1 => {
                                let (sx, sy) = (r.read_u16().await?, r.read_u16().await?);
                                let copy = region(&screen, width, height, sx, sy, rw, rh);
                                blit(&mut screen, width, x, y, rw, rh, &copy);
                            }
                            16 => {
                                let len = r.read_u32().await?;
                                let mut data = vec![0u8; len as usize];
                                r.read_exact(&mut data).await?;
                                let raw = inflate(&mut zlib, &data)?;
                                let px = zrle(&raw, rw, rh)?;
                                blit(&mut screen, width, x, y, rw, rh, &px);
                            }
                            other => return Err(fail(format!("the server sent an encoding Linecard didn't ask for ({other})"))),
                        }
                        let rgba = region(&screen, width, height, x, y, rw, rh);
                        emit(GfxEvent::Frame { x, y, width: rw, height: rh, rgba });
                    }
                    w.write_all(&update_request(true, width, height)).await?;
                }
                1 => {
                    // Colour map: unused with true color; skip it.
                    let _pad = r.read_u8().await?;
                    let _first = r.read_u16().await?;
                    let n = r.read_u16().await?;
                    let mut skip = vec![0u8; usize::from(n) * 6];
                    r.read_exact(&mut skip).await?;
                }
                2 => emit(GfxEvent::Bell),
                3 => {
                    let mut pad = [0u8; 3];
                    r.read_exact(&mut pad).await?;
                    let len = r.read_u32().await?;
                    let mut text = vec![0u8; len.min(1 << 20) as usize];
                    r.read_exact(&mut text).await?;
                    // RFB clipboard text is Latin-1.
                    emit(GfxEvent::Clipboard(text.iter().map(|&b| b as char).collect()));
                }
                other => return Err(fail(format!("unexpected message from the server ({other})"))),
            },
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Pointer(x, y, b)) => {
                    buttons = b;
                    w.write_all(&pointer(x, y, b)).await?;
                }
                Some(Cmd::Wheel(x, y, dx, dy)) => {
                    // Buttons 4/5 scroll up/down, 6/7 left/right: press and release.
                    let mut out = Vec::new();
                    for (count, bit) in [(-dy, 8u8), (dy, 16), (-dx, 32), (dx, 64)] {
                        for _ in 0..count.max(0) {
                            out.extend(pointer(x, y, buttons | bit));
                            out.extend(pointer(x, y, buttons));
                        }
                    }
                    w.write_all(&out).await?;
                }
                Some(Cmd::Key(k, down)) => w.write_all(&key(k, down)).await?,
                Some(Cmd::Text(t)) => {
                    let mut out = Vec::new();
                    for c in t.chars() {
                        let k = if c == '\n' { Some(0xff0d) } else { keysym("", &c.to_string()) };
                        if let Some(k) = k {
                            out.extend(key(k, true));
                            out.extend(key(k, false));
                        }
                    }
                    w.write_all(&out).await?;
                }
                Some(Cmd::Clipboard(t)) => w.write_all(&client_cut_text(&t)).await?,
                Some(Cmd::Close) | None => return Ok(()),
            },
        }
    }
}

async fn reason<R: AsyncReadExt + Unpin>(r: &mut R) -> String {
    let Ok(n) = r.read_u32().await else {
        return "connection refused".into();
    };
    let mut text = vec![0u8; n.min(4096) as usize];
    match r.read_exact(&mut text).await {
        Ok(_) => String::from_utf8_lossy(&text).into_owned(),
        Err(_) => "connection refused".into(),
    }
}

fn update_request(incremental: bool, w: u16, h: u16) -> Vec<u8> {
    let mut m = vec![3u8, incremental as u8, 0, 0, 0, 0];
    m.extend(w.to_be_bytes());
    m.extend(h.to_be_bytes());
    m
}

fn pointer(x: u16, y: u16, buttons: Buttons) -> Vec<u8> {
    let mut m = vec![5u8, buttons];
    m.extend(x.to_be_bytes());
    m.extend(y.to_be_bytes());
    m
}

fn key(sym: u32, down: bool) -> Vec<u8> {
    let mut m = vec![4u8, down as u8, 0, 0];
    m.extend(sym.to_be_bytes());
    m
}

/// Copy a `w`×`h` RGBA block into the screen at (x, y), clipped.
fn blit(screen: &mut [u8], sw: u16, x: u16, y: u16, w: u16, h: u16, px: &[u8]) {
    let sw = usize::from(sw);
    let sh = screen.len() / 4 / sw.max(1);
    for row in 0..usize::from(h) {
        let ty = usize::from(y) + row;
        if ty >= sh {
            break;
        }
        let cols = usize::from(w).min(sw.saturating_sub(usize::from(x)));
        let src = row * usize::from(w) * 4;
        let dst = (ty * sw + usize::from(x)) * 4;
        screen[dst..dst + cols * 4].copy_from_slice(&px[src..src + cols * 4]);
    }
}

/// Read a `w`×`h` RGBA block out of the screen (zeros outside it).
fn region(screen: &[u8], sw: u16, sh: u16, x: u16, y: u16, w: u16, h: u16) -> Vec<u8> {
    let mut out = vec![0u8; usize::from(w) * usize::from(h) * 4];
    for row in 0..usize::from(h) {
        let sy = usize::from(y) + row;
        if sy >= usize::from(sh) {
            break;
        }
        let cols = usize::from(w).min(usize::from(sw).saturating_sub(usize::from(x)));
        let src = (sy * usize::from(sw) + usize::from(x)) * 4;
        let dst = row * usize::from(w) * 4;
        out[dst..dst + cols * 4].copy_from_slice(&screen[src..src + cols * 4]);
    }
    out
}

/// One ZRLE rectangle's zlib data, continuing the connection's stream.
fn inflate(z: &mut Decompress, data: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4);
    let mut input = data;
    loop {
        out.reserve(64 * 1024);
        let before_in = z.total_in();
        z.decompress_vec(input, &mut out, FlushDecompress::Sync)
            .map_err(|e| fail(format!("bad ZRLE data: {e}")))?;
        let used = (z.total_in() - before_in) as usize;
        input = &input[used..];
        if input.is_empty() && out.len() < out.capacity() {
            return Ok(out);
        }
        if used == 0 && out.len() < out.capacity() {
            return Ok(out);
        }
    }
}

/// Decode ZRLE tiles (64×64, row by row) into RGBA. CPIXELs are 3 bytes
/// (R, G, B) with the pixel format requested above.
pub fn zrle(data: &[u8], w: u16, h: u16) -> io::Result<Vec<u8>> {
    let (w, h) = (usize::from(w), usize::from(h));
    let mut out = vec![0u8; w * h * 4];
    let mut p = 0usize;
    let short = || fail("ZRLE data ended early");
    let take = |n: usize, p: &mut usize| -> io::Result<&[u8]> {
        let s = data.get(*p..*p + n).ok_or_else(short)?;
        *p += n;
        Ok(s)
    };
    let mut ty = 0;
    while ty < h {
        let th = 64.min(h - ty);
        let mut tx = 0;
        while tx < w {
            let tw = 64.min(w - tx);
            let mut tile = vec![[0u8; 3]; tw * th];
            let sub = take(1, &mut p)?[0];
            match sub {
                0 => {
                    let raw = take(tw * th * 3, &mut p)?;
                    for (i, px) in tile.iter_mut().enumerate() {
                        px.copy_from_slice(&raw[i * 3..i * 3 + 3]);
                    }
                }
                1 => {
                    let c = take(3, &mut p)?;
                    let c = [c[0], c[1], c[2]];
                    tile.fill(c);
                }
                2..=16 => {
                    let n = usize::from(sub);
                    let pal: Vec<[u8; 3]> = take(n * 3, &mut p)?
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|c| [c[0], c[1], c[2]])
                        .collect();
                    let bits = match n {
                        2 => 1,
                        3 | 4 => 2,
                        _ => 4,
                    };
                    let row_bytes = (tw * bits).div_ceil(8);
                    let packed = take(row_bytes * th, &mut p)?;
                    for row in 0..th {
                        for col in 0..tw {
                            let bit = col * bits;
                            let byte = packed[row * row_bytes + bit / 8];
                            let idx = (byte >> (8 - bits - bit % 8)) & ((1 << bits) - 1);
                            tile[row * tw + col] = *pal
                                .get(usize::from(idx))
                                .ok_or_else(|| fail("bad ZRLE palette index"))?;
                        }
                    }
                }
                128 | 130..=255 => {
                    let pal: Vec<[u8; 3]> = if sub == 128 {
                        vec![]
                    } else {
                        take(usize::from(sub - 128) * 3, &mut p)?
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .map(|c| [c[0], c[1], c[2]])
                            .collect()
                    };
                    let mut i = 0;
                    while i < tw * th {
                        let (color, run) = if sub == 128 {
                            let c = take(3, &mut p)?;
                            ([c[0], c[1], c[2]], true)
                        } else {
                            let idx = take(1, &mut p)?[0];
                            let c = *pal
                                .get(usize::from(idx & 127))
                                .ok_or_else(|| fail("bad ZRLE palette index"))?;
                            (c, idx & 128 != 0)
                        };
                        let mut len = 1;
                        if run {
                            loop {
                                let b = take(1, &mut p)?[0];
                                len += usize::from(b);
                                if b != 255 {
                                    break;
                                }
                            }
                        }
                        for _ in 0..len.min(tw * th - i) {
                            tile[i] = color;
                            i += 1;
                        }
                    }
                }
                _ => return Err(fail(format!("bad ZRLE tile type {sub}"))),
            }
            for row in 0..th {
                for col in 0..tw {
                    let c = tile[row * tw + col];
                    let o = ((ty + row) * w + tx + col) * 4;
                    out[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
            tx += 64;
        }
        ty += 64;
    }
    Ok(out)
}
