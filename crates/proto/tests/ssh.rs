//! End-to-end tests for the SSH driver against an in-process russh server
//! on 127.0.0.1: host-key prompts and known_hosts, password and
//! keyboard-interactive auth, shell I/O, resize, cancel and close.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linecard_proto::ssh::{JumpChain, SshSession};
use linecard_proto::{
    Credentials, EventSink, ForwardKind, HostKeyDecision, HostKeyStatus, PortForward, PromptReply, Session,
    SessionEvent, SshOptions, TerminalTarget,
};
use russh::client::GexParams;
use russh::kex::dh::groups::{DhGroup, DH_GROUP14};
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, PublicKeyOffer, Response, VerifiedPublicKey};
use russh::{cipher, kex, mac, Channel, ChannelId, MethodKind, MethodSet, Preferred};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// Throwaway key generated for these tests only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDtwJ0rd6G1qUoAaxjzytAz1RP/966yrWAwGQZtmZ0IegAAAJguX39ILl9/
SAAAAAtzc2gtZWQyNTUxOQAAACDtwJ0rd6G1qUoAaxjzytAz1RP/966yrWAwGQZtmZ0Ieg
AAAEA8hph6w7PFXOL7C8KMMrkeSeLXCzA6XIPUsjOyk0gQk+3AnSt3obWpSgBrGPPK0DPV
E//3rrKtYDAZBm2ZnQh6AAAAEmxpbmVjYXJkLXRlc3QtaG9zdAECAw==
-----END OPENSSH PRIVATE KEY-----
";
const HOST_FINGERPRINT: &str = "SHA256:iVxAQiFJZeZYlU9KKfVhxXsCjnnmVyysNOGExA17hCU";
const HOST_PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO3AnSt3obWpSgBrGPPK0DPVE//3rrKtYDAZBm2ZnQh6";
const OTHER_PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICRKKT184R+EfAIXretwOCSEPkE8eN7b3FFdjjTs3VO6";

/// A user key protected by the passphrase "open sesame" (throwaway, tests only).
const USER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABCZfEKMHA
GF34YEyIQY8K5kAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAINl6L25yBTGVbJ/l
fx+Dy1jI4CbQQEHi0Ii96aKs7PkdAAAAoMaip1mgNDGy0wDzntUygL/W0JBw6FnAa7JcZM
a24xA8c2U79xl+k93n7qJATXhlXNHxTHLlTNR5ORXxH6zr7YdqupiULVNeyKNtG8WhlU6p
mMRAVFLSbTIOnKNst5I/TRSeAD6eZPCnD1JaP33pGkccKCKeZt39bceLdvbuK3MCl1blR7
15nkaMJc0fwOU5B5bbaY65P6TjALh7hop1InQ=
-----END OPENSSH PRIVATE KEY-----
";
const USER_PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINl6L25yBTGVbJ/lfx+Dy1jI4CbQQEHi0Ii96aKs7Pkd";

// ---- test server ---------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Mode {
    Password,
    KeyboardInteractive,
    /// Keyboard-interactive only, turned down at once without a prompt, as
    /// OpenSSH does for an account PAM has locked after failed logins.
    LockedOut,
    /// Public key or password; the key is authorized only if .
    KeyOrPassword(bool),
    /// Password, and group exchange as old Cisco IOS does it: only a 2048-bit
    /// group, and the connection is dropped if the client prefers or needs
    /// a bigger one.
    OldIos,
}

struct Shell {
    authorized: Option<PublicKey>,
    locked: bool,
    old_ios: bool,
    /// Session channels; forwarded (jump) channels must not be echoed.
    shells: std::collections::HashSet<ChannelId>,
}

impl Shell {
    fn takes(&self, key: &PublicKey) -> bool {
        self.authorized.as_ref().is_some_and(|k| k.key_data() == key.key_data())
    }
}

/// Logins the locked-out server turned down.
static LOCKED_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

/// Reject but keep offering the method, as OpenSSH and network gear do
/// (russh's server would otherwise drop it after one failure).
fn retry(method: MethodKind) -> Auth {
    Auth::Reject {
        proceed_with_methods: Some(MethodSet::from(&[method][..])),
        partial_success: false,
    }
}

impl server::Handler for Shell {
    type Error = russh::Error;

    async fn lookup_dh_gex_group(&mut self, gex: &GexParams) -> Result<Option<DhGroup>, Self::Error> {
        if self.old_ios && (gex.min_group_size() > 2048 || gex.preferred_group_size() > 2048) {
            return Ok(None);
        }
        Ok(Some(DH_GROUP14))
    }

    /// Like a real server: say yes to the probe only for an authorized key.
    async fn auth_publickey_offered(&mut self, _user: &str, offer: PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        Ok(if self.takes(offer.public_key) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, _user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if self.takes(key.public_key) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "admin" && password == "secret" {
            Auth::Accept
        } else {
            retry(MethodKind::Password)
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if self.locked {
            LOCKED_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
            return Ok(retry(MethodKind::KeyboardInteractive));
        }
        Ok(match response {
            None => Auth::Partial {
                name: "".into(),
                instructions: "Use your lab credentials".into(),
                prompts: vec![("Password: ".into(), false)].into(),
            },
            Some(mut answers) => {
                if user == "admin" && answers.next().as_deref() == Some(&b"secret"[..]) {
                    Auth::Accept
                } else {
                    retry(MethodKind::KeyboardInteractive)
                }
            }
        })
    }

    /// Act as a jump host: forward the channel to host:port.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _from: &str,
        _from_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let Ok(mut upstream) = tokio::net::TcpStream::connect((host, port as u16)).await else {
            return Ok(()); // dropping `reply` rejects the channel
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        });
        Ok(())
    }

    /// Remote forwarding: listen, and send each connection back to the client.
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        let host = if address == "localhost" { "127.0.0.1" } else { address };
        let Ok(listener) = TcpListener::bind((host, *port as u16)).await else {
            return Ok(false);
        };
        *port = listener.local_addr().unwrap().port().into();
        let (bound, handle, address) = (*port, session.handle(), address.to_string());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (handle, address) = (handle.clone(), address.clone());
                tokio::spawn(async move {
                    if let Ok(ch) = handle
                        .channel_open_forwarded_tcpip(address, bound, "127.0.0.1", 1)
                        .await
                    {
                        let mut stream = ch.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
                    }
                });
            }
        });
        Ok(true)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.shells.insert(channel.id());
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("term={term} {cols}x{rows}\r\n"))?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut server::Session) -> Result<(), Self::Error> {
        session.data(channel, "welcome\r\n$ ")?;
        Ok(())
    }

    /// Echo input upper-cased, so the test can tell it came from the server.
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.shells.contains(&channel) {
            session.data(channel, data.to_ascii_uppercase())?;
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("resized {cols}x{rows}\r\n"))?;
        Ok(())
    }
}

async fn start_server(mode: Mode) -> u16 {
    start_server_with(mode, Preferred::DEFAULT).await
}

async fn start_server_with(mode: Mode, preferred: Preferred) -> u16 {
    let methods: &[MethodKind] = match mode {
        Mode::Password | Mode::OldIos => &[MethodKind::Password],
        Mode::KeyboardInteractive | Mode::LockedOut => &[MethodKind::KeyboardInteractive],
        Mode::KeyOrPassword(_) => &[MethodKind::PublicKey, MethodKind::Password],
    };
    let authorized = match mode {
        Mode::KeyOrPassword(true) => Some(PublicKey::from_openssh(USER_PUBLIC).unwrap()),
        _ => None,
    };
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from_openssh(HOST_KEY).unwrap()],
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::from_millis(5),
        auth_rejection_time_initial: Some(Duration::ZERO),
        preferred,
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let shell = Shell {
                authorized: authorized.clone(),
                locked: matches!(mode, Mode::LockedOut),
                old_ios: matches!(mode, Mode::OldIos),
                shells: Default::default(),
            };
            tokio::spawn(async move {
                if let Ok(running) = server::run_stream(config, stream, shell).await {
                    let _ = running.await;
                }
            });
        }
    });
    port
}

// ---- client side helpers --------------------------------------------------------

fn temp_known_hosts() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "linecard-ssh-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("known_hosts")
}

fn options(known_hosts: PathBuf) -> SshOptions {
    // No agent or identity files: the developer's own keys must not leak into tests.
    SshOptions {
        credentials: None,
        known_hosts,
        extra_known_hosts: vec![],
        identity_files: vec![],
        use_agent: false,
    }
}

/// In-memory stand-in for the OS keychain.
#[derive(Debug, Default)]
struct MemoryCreds(Mutex<HashMap<String, String>>);

impl Credentials for MemoryCreds {
    fn password(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn save_password(&self, key: &str, _label: &str, password: &str) {
        self.0.lock().unwrap().insert(key.into(), password.into());
    }
}

struct Probe {
    session: SshSession,
    events: UnboundedReceiver<SessionEvent>,
    /// Terminal output seen so far.
    screen: String,
}

fn connect(port: u16, user: Option<&str>, opts: &SshOptions) -> Probe {
    let (tx, events) = unbounded_channel();
    let sink: EventSink = Box::new(move |e| {
        let _ = tx.send(e);
    });
    let mut target = TerminalTarget::new("127.0.0.1", port);
    target.username = user.map(Into::into);
    Probe {
        session: SshSession::connect(&target, opts, sink).unwrap(),
        events,
        screen: String::new(),
    }
}

impl Probe {
    /// Next non-data event; output on the way is appended to `screen`.
    async fn event(&mut self) -> SessionEvent {
        loop {
            let e = tokio::time::timeout(Duration::from_secs(10), self.events.recv())
                .await
                .expect("timed out waiting for a session event")
                .expect("event stream ended");
            match e {
                SessionEvent::Data(d) => self.screen.push_str(&String::from_utf8_lossy(&d)),
                other => return other,
            }
        }
    }

    /// Wait until the terminal shows `text`.
    async fn screen_has(&mut self, text: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !self.screen.contains(text) {
            let e = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("never saw {text:?}; screen: {:?}", self.screen))
                .expect("event stream ended");
            if let SessionEvent::Data(d) = e {
                self.screen.push_str(&String::from_utf8_lossy(&d));
            }
        }
    }

    fn reply(&mut self, r: PromptReply) {
        self.session.reply(r).unwrap();
    }

    fn password(&mut self, p: &str) {
        self.reply(PromptReply::Fields {
            values: vec![p.into()],
            remember: false,
        });
    }
}

// ---- tests -------------------------------------------------------------------

#[tokio::test]
async fn password_login_shell_and_known_hosts() {
    let port = start_server(Mode::Password).await;
    let known = temp_known_hosts();
    let opts = options(known.clone());
    let mut c = connect(port, Some("admin"), &opts);

    // First contact: unknown key with the right fingerprint.
    match c.event().await {
        SessionEvent::HostKey(k) => {
            assert_eq!(k.status, HostKeyStatus::Unknown);
            assert_eq!(k.algorithm, "ssh-ed25519");
            assert_eq!(k.fingerprint, HOST_FINGERPRINT);
            assert_eq!((k.host.as_str(), k.port), ("127.0.0.1", port));
        }
        other => panic!("expected a host key prompt, got {other:?}"),
    }
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptAndSave));

    // A wrong password is retried with a visible "Access denied".
    match c.event().await {
        SessionEvent::Auth(p) => {
            assert_eq!(p.title, "Password for admin@127.0.0.1");
            assert_eq!(p.fields.len(), 1);
            assert!(!p.fields[0].echo, "passwords are not echoed");
        }
        other => panic!("expected a password prompt, got {other:?}"),
    }
    c.password("wrong");
    let retry = c.event().await;
    assert!(
        matches!(retry, SessionEvent::Auth(_)),
        "got {retry:?}, screen {:?}",
        c.screen
    );
    assert!(c.screen.contains("Access denied"));
    c.password("secret");

    assert_eq!(c.event().await, SessionEvent::Connected);
    c.screen_has("term=xterm-256color 80x24").await;
    c.screen_has("welcome").await;

    c.session.write(b"show version").unwrap();
    c.screen_has("SHOW VERSION").await;
    c.session.resize(132, 40).unwrap();
    c.screen_has("resized 132x40").await;

    c.session.close().unwrap();
    assert_eq!(c.event().await, SessionEvent::Disconnected { reason: None });

    let saved = std::fs::read_to_string(&known).unwrap();
    assert!(
        saved.contains(&format!("[127.0.0.1]:{port} {HOST_PUBLIC}")),
        "known_hosts: {saved}"
    );

    // Second connection: the key is known, so straight to the password.
    let mut again = connect(port, Some("admin"), &opts);
    assert!(matches!(again.event().await, SessionEvent::Auth(_)));
    again.session.close().unwrap();
}

#[tokio::test]
async fn keyboard_interactive_asks_for_username_first() {
    let port = start_server(Mode::KeyboardInteractive).await;
    let opts = options(temp_known_hosts());
    let mut c = connect(port, None, &opts);

    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));

    match c.event().await {
        SessionEvent::Auth(p) => {
            assert_eq!(p.fields[0].label, "Username");
            assert!(p.fields[0].echo);
        }
        other => panic!("expected a username prompt, got {other:?}"),
    }
    c.reply(PromptReply::Fields {
        values: vec!["admin".into()],
        remember: false,
    });

    match c.event().await {
        SessionEvent::Auth(p) => {
            assert_eq!(p.instructions, "Use your lab credentials");
            assert_eq!(p.fields[0].label, "Password", "trailing colon trimmed");
            assert!(!p.fields[0].echo);
        }
        other => panic!("expected a keyboard-interactive prompt, got {other:?}"),
    }
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.close().unwrap();

    // AcceptOnce leaves known_hosts alone.
    assert!(!opts.known_hosts.exists());
}

#[tokio::test]
async fn a_server_that_refuses_without_asking_is_not_retried() {
    // A locked account (wrong password too often): OpenSSH turns
    // keyboard-interactive down without a prompt. Retrying can't help and
    // can restart the lockout, so the session says why and ends at once.
    let port = start_server(Mode::LockedOut).await;
    let opts = options(temp_known_hosts());
    let mut c = connect(port, Some("admin"), &opts);
    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));

    match c.event().await {
        SessionEvent::Disconnected { reason: Some(r) } => {
            assert!(r.contains("refused the login without asking for a password"), "{r}")
        }
        other => panic!("expected the session to end, got {other:?}"),
    }
    assert!(!c.screen.contains("Access denied"), "screen: {:?}", c.screen);
    assert_eq!(LOCKED_ATTEMPTS.load(Ordering::SeqCst), 1, "tried once, not three times");
}

#[tokio::test]
async fn old_cisco_switches_connect() {
    let cases = [
        // A Catalyst 3750 on IOS 12.2 (Cisco-1.25), exactly.
        (
            vec![kex::DH_GEX_SHA1, kex::DH_G14_SHA1, kex::DH_G1_SHA1],
            vec![
                cipher::AES_128_CBC,
                cipher::TRIPLE_DES_CBC,
                cipher::AES_192_CBC,
                cipher::AES_256_CBC,
            ],
        ),
        // Group exchange alone: the group asked for must be one it has.
        (vec![kex::DH_GEX_SHA1], vec![cipher::AES_128_CBC]),
        // Older still: group1 and 3des-cbc only.
        (vec![kex::DH_G1_SHA1], vec![cipher::TRIPLE_DES_CBC]),
    ];
    for (kexes, ciphers) in cases {
        let what = format!("{kexes:?}");
        let port = start_server_with(
            Mode::OldIos,
            Preferred {
                kex: kexes.into(),
                cipher: ciphers.into(),
                mac: vec![mac::HMAC_SHA1].into(),
                ..Preferred::DEFAULT
            },
        )
        .await;
        let opts = options(temp_known_hosts());
        let mut c = connect(port, Some("admin"), &opts);
        let first = c.event().await;
        assert!(matches!(first, SessionEvent::HostKey(_)), "{what}: {first:?}");
        c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
        assert!(matches!(c.event().await, SessionEvent::Auth(_)), "{what}");
        c.password("secret");
        assert_eq!(c.event().await, SessionEvent::Connected, "{what}");
        c.screen_has("welcome").await;
        c.session.close().unwrap();
    }
}

#[tokio::test]
async fn a_server_that_hangs_up_mid_handshake_is_explained() {
    // Old IOS closes the connection when it shares no algorithm with the
    // client, rather than saying so.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_all(b"SSH-1.99-Cisco-1.25\r\n").await.unwrap();
        let mut buf = [0u8; 256];
        let _ = s.read(&mut buf).await;
        // Close our side cleanly, and drain theirs so Windows doesn't turn
        // the close into a reset.
        let _ = s.shutdown().await;
        while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
    });
    let opts = options(temp_known_hosts());
    let mut c = connect(port, Some("admin"), &opts);
    match c.event().await {
        SessionEvent::Disconnected { reason: Some(r) } => {
            assert!(r.contains("closed the connection during the SSH handshake"), "{r}")
        }
        other => panic!("expected the session to end, got {other:?}"),
    }
}

#[tokio::test]
async fn changed_host_key_warns_and_can_be_replaced() {
    let port = start_server(Mode::Password).await;
    let known = temp_known_hosts();
    std::fs::write(&known, format!("[127.0.0.1]:{port} {OTHER_PUBLIC}\n")).unwrap();
    let opts = options(known.clone());

    // Rejecting a changed key ends the session.
    let mut c = connect(port, Some("admin"), &opts);
    match c.event().await {
        SessionEvent::HostKey(k) => assert_eq!(
            k.status,
            HostKeyStatus::Changed {
                known_hosts: known.clone(),
                line: 1
            }
        ),
        other => panic!("expected a changed-key prompt, got {other:?}"),
    }
    c.reply(PromptReply::HostKey(HostKeyDecision::Reject));
    assert_eq!(
        c.event().await,
        SessionEvent::Disconnected {
            reason: Some("host key not accepted".into())
        }
    );

    // Accepting and saving replaces the old line instead of adding a second one.
    let mut c = connect(port, Some("admin"), &opts);
    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptAndSave));
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.session.close().unwrap();

    let saved = std::fs::read_to_string(&known).unwrap();
    assert!(!saved.contains(OTHER_PUBLIC), "old key removed: {saved}");
    assert_eq!(saved.matches(HOST_PUBLIC).count(), 1, "new key saved once: {saved}");

    let mut c = connect(port, Some("admin"), &opts);
    assert!(
        matches!(c.event().await, SessionEvent::Auth(_)),
        "no prompt after replacing"
    );
    c.session.close().unwrap();
}

/// `|1|salt|hash`, the way `HashKnownHosts yes` writes a host name.
fn hashed(host_port: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let salt = [7u8; 20];
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
    let hash = ring::hmac::sign(&key, host_port.as_bytes());
    format!("|1|{}|{}", STANDARD.encode(salt), STANDARD.encode(hash.as_ref()))
}

#[tokio::test]
async fn replacing_a_key_keeps_the_rest_of_the_file() {
    let port = start_server(Mode::Password).await;
    let known = temp_known_hosts();
    let me = format!("[127.0.0.1]:{port}");
    let original = format!(
        "# kept by hand\r\n\
         other.lab {HOST_PUBLIC}\r\n\
         {me},router.lab {OTHER_PUBLIC}\r\n\
         {} {OTHER_PUBLIC}\r\n",
        hashed(&me)
    );
    std::fs::write(&known, &original).unwrap();
    let opts = options(known.clone());

    let mut c = connect(port, Some("admin"), &opts);
    match c.event().await {
        // The real line, counting the comment.
        SessionEvent::HostKey(k) => assert_eq!(
            k.status,
            HostKeyStatus::Changed {
                known_hosts: known.clone(),
                line: 3
            }
        ),
        other => panic!("expected a changed-key prompt, got {other:?}"),
    }
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptAndSave));
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.session.close().unwrap();

    let saved = std::fs::read_to_string(&known).unwrap();
    // Comment, other hosts, the alias and CRLF kept; both old entries for
    // this host (plain and hashed) gone; the new key added once at the end.
    let expected = format!(
        "# kept by hand\r\n\
         other.lab {HOST_PUBLIC}\r\n\
         router.lab {OTHER_PUBLIC}\r\n\
         {me} {HOST_PUBLIC}\n"
    );
    assert_eq!(saved, expected);
    assert!(!known.with_file_name("known_hosts.linecard-tmp").exists());

    let mut c = connect(port, Some("admin"), &opts);
    assert!(
        matches!(c.event().await, SessionEvent::Auth(_)),
        "no prompt after replacing"
    );
    c.session.close().unwrap();
}

#[tokio::test]
async fn saved_passwords_are_offered_used_and_updated() {
    for mode in [Mode::Password, Mode::KeyboardInteractive] {
        let port = start_server(mode).await;
        let creds = Arc::new(MemoryCreds::default());
        let mut opts = options(temp_known_hosts());
        opts.credentials = Some(creds.clone());
        let key = format!("ssh:admin@127.0.0.1:{port}");

        // First login: "Save password" offered unticked; ticking it saves.
        let mut c = connect(port, Some("admin"), &opts);
        assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
        c.reply(PromptReply::HostKey(HostKeyDecision::AcceptAndSave));
        match c.event().await {
            SessionEvent::Auth(p) => assert_eq!(p.remember, Some(false)),
            other => panic!("expected a password prompt, got {other:?}"),
        }
        c.reply(PromptReply::Fields {
            values: vec!["secret".into()],
            remember: true,
        });
        assert_eq!(c.event().await, SessionEvent::Connected);
        c.session.close().unwrap();
        assert_eq!(creds.password(&key).as_deref(), Some("secret"));

        // Next login uses it without asking.
        let mut c = connect(port, Some("admin"), &opts);
        assert_eq!(
            c.event().await,
            SessionEvent::Connected,
            "{mode:?}: no prompt with a saved password"
        );
        c.session.close().unwrap();

        // The device's password changed: say so, ask with the box ticked, update.
        creds.save_password(&key, "admin@127.0.0.1", "old-password");
        let mut c = connect(port, Some("admin"), &opts);
        match c.event().await {
            SessionEvent::Auth(p) => assert_eq!(p.remember, Some(true)),
            other => panic!("expected a re-prompt, got {other:?}"),
        }
        assert!(c.screen.contains("Saved password was not accepted"), "{}", c.screen);
        c.reply(PromptReply::Fields {
            values: vec!["secret".into()],
            remember: true,
        });
        assert_eq!(c.event().await, SessionEvent::Connected);
        c.session.close().unwrap();
        assert_eq!(creds.password(&key).as_deref(), Some("secret"));
    }
}

#[tokio::test]
async fn unticked_passwords_are_not_saved() {
    let port = start_server(Mode::Password).await;
    let creds = Arc::new(MemoryCreds::default());
    let mut opts = options(temp_known_hosts());
    opts.credentials = Some(creds.clone());
    let mut c = connect(port, Some("admin"), &opts);
    c.event().await;
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    c.event().await;
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    assert!(creds.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn jump_host_tunnels_to_the_target() {
    let jump_port = start_server(Mode::Password).await;
    let target_port = start_server(Mode::KeyboardInteractive).await;
    let opts = options(temp_known_hosts());

    let mut jump = TerminalTarget::new("127.0.0.1", jump_port);
    jump.username = Some("admin".into());
    let mut target = TerminalTarget::new("127.0.0.1", target_port);
    target.username = Some("admin".into());
    target.jump = Some(Box::new(jump));

    let (tx, events) = unbounded_channel();
    let sink: EventSink = Box::new(move |e| {
        let _ = tx.send(e);
    });
    let mut c = Probe {
        session: SshSession::connect(&target, &opts, sink).unwrap(),
        events,
        screen: String::new(),
    };

    // The jump host first: its own host key and password...
    match c.event().await {
        SessionEvent::HostKey(k) => assert_eq!(k.port, jump_port),
        other => panic!("expected the jump host's key, got {other:?}"),
    }
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    match c.event().await {
        SessionEvent::Auth(p) => assert_eq!(p.title, "Password for admin@127.0.0.1"),
        other => panic!("expected the jump host's password, got {other:?}"),
    }
    c.password("secret");

    // ...then the target, reached through the tunnel.
    match c.event().await {
        SessionEvent::HostKey(k) => assert_eq!(k.port, target_port),
        other => panic!("expected the target's key, got {other:?}"),
    }
    assert!(c.screen.contains("Via jump host 127.0.0.1"), "{}", c.screen);
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.write(b"through the tunnel").unwrap();
    c.screen_has("THROUGH THE TUNNEL").await;
    c.session.close().unwrap();
}

#[tokio::test]
async fn unreachable_target_behind_a_jump_host_says_so() {
    let jump_port = start_server(Mode::Password).await;
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let opts = options(temp_known_hosts());
    let mut jump = TerminalTarget::new("127.0.0.1", jump_port);
    jump.username = Some("admin".into());
    let mut target = TerminalTarget::new("127.0.0.1", closed);
    target.jump = Some(Box::new(jump));
    let (tx, events) = unbounded_channel();
    let sink: EventSink = Box::new(move |e| {
        let _ = tx.send(e);
    });
    let mut c = Probe {
        session: SshSession::connect(&target, &opts, sink).unwrap(),
        events,
        screen: String::new(),
    };
    c.event().await;
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    c.event().await;
    c.password("secret");
    match c.event().await {
        SessionEvent::Disconnected { reason: Some(r) } => {
            assert!(r.starts_with("jump host could not reach 127.0.0.1"), "{r}")
        }
        other => panic!("expected a tunnel error, got {other:?}"),
    }
}

/// What VNC and RDP use to reach a host behind a bastion: log in once, open
/// TCP streams from the jump host.
#[tokio::test]
async fn jump_chain_opens_streams_for_other_protocols() {
    let jump_port = start_server(Mode::Password).await;
    let echo = echo_server().await;
    let opts = options(temp_known_hosts());
    let mut jump = TerminalTarget::new("127.0.0.1", jump_port);
    jump.username = Some("admin".into());

    let (tx, mut events) = unbounded_channel();
    let sink: EventSink = Box::new(move |e| {
        let _ = tx.send(e);
    });
    let (reply_tx, reply_rx) = unbounded_channel();
    let replies = Arc::new(tokio::sync::Mutex::new(reply_rx));
    let opening = tokio::spawn(async move { JumpChain::open(&jump, &opts, sink, replies).await });

    let mut next = async || {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("timed out waiting for a prompt")
            .expect("events ended")
    };
    assert!(matches!(next().await, SessionEvent::HostKey(_)));
    reply_tx
        .send(PromptReply::HostKey(HostKeyDecision::AcceptOnce))
        .unwrap();
    assert!(matches!(next().await, SessionEvent::Auth(_)));
    reply_tx
        .send(PromptReply::Fields {
            values: vec!["secret".into()],
            remember: false,
        })
        .unwrap();
    let chain = opening.await.unwrap().expect("jump login");

    // Each connection is its own channel through the one login.
    for _ in 0..2 {
        let mut s = chain.connect("127.0.0.1", echo).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut buf))
            .await
            .expect("no echo through the jump host")
            .unwrap();
        assert_eq!(&buf, b"ping");
    }
    let closed = free_port();
    let err = chain.connect("127.0.0.1", closed).await.err().expect("closed port");
    assert!(
        err.to_string().starts_with("jump host could not reach 127.0.0.1"),
        "{err}"
    );
}

/// Options with the encrypted user key as the only identity file.
fn options_with_key(creds: Option<Arc<MemoryCreds>>) -> (SshOptions, String) {
    let known = temp_known_hosts();
    let key_path = known.with_file_name("id_ed25519");
    std::fs::write(&key_path, USER_KEY).unwrap();
    let mut opts = options(known);
    opts.identity_files = vec![key_path.clone()];
    opts.credentials = creds.map(|c| c as Arc<dyn Credentials>);
    (opts, format!("keyfile:{}", key_path.display()))
}

async fn past_host_key(c: &mut Probe) {
    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
}

#[tokio::test]
async fn encrypted_key_asks_for_its_passphrase_when_the_server_takes_it() {
    let port = start_server(Mode::KeyOrPassword(true)).await;
    let creds = Arc::new(MemoryCreds::default());
    let (opts, slot) = options_with_key(Some(creds.clone()));

    let mut c = connect(port, Some("admin"), &opts);
    past_host_key(&mut c).await;
    match c.event().await {
        SessionEvent::Auth(p) => {
            assert_eq!(p.title, "Passphrase for id_ed25519");
            assert_eq!(p.remember, Some(false));
        }
        other => panic!("expected a passphrase prompt, got {other:?}"),
    }
    c.password("not it");
    match c.event().await {
        SessionEvent::Auth(p) => assert_eq!(p.instructions, "Wrong passphrase, try again."),
        other => panic!("expected a retry, got {other:?}"),
    }
    c.reply(PromptReply::Fields {
        values: vec!["open sesame".into()],
        remember: true,
    });
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.close().unwrap();
    assert_eq!(creds.password(&slot).as_deref(), Some("open sesame"));

    // The saved passphrase unlocks it next time without asking.
    let mut c = connect(port, Some("admin"), &opts);
    past_host_key(&mut c).await;
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.close().unwrap();
}

#[tokio::test]
async fn unaccepted_keys_never_ask_for_a_passphrase() {
    let port = start_server(Mode::KeyOrPassword(false)).await;
    let (opts, _) = options_with_key(None);
    let mut c = connect(port, Some("admin"), &opts);
    past_host_key(&mut c).await;
    match c.event().await {
        SessionEvent::Auth(p) => assert_eq!(p.title, "Password for admin@127.0.0.1", "straight to the password"),
        other => panic!("expected a password prompt, got {other:?}"),
    }
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.close().unwrap();
}

#[tokio::test]
async fn canceling_a_passphrase_falls_back_to_password() {
    let port = start_server(Mode::KeyOrPassword(true)).await;
    let (opts, _) = options_with_key(None);
    let mut c = connect(port, Some("admin"), &opts);
    past_host_key(&mut c).await;
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.reply(PromptReply::Cancel);
    match c.event().await {
        SessionEvent::Auth(p) => assert_eq!(p.title, "Password for admin@127.0.0.1"),
        other => panic!("expected a password prompt after skipping the key, got {other:?}"),
    }
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    c.session.close().unwrap();
}

#[tokio::test]
async fn canceling_a_prompt_disconnects() {
    let port = start_server(Mode::Password).await;
    let opts = options(temp_known_hosts());
    let mut c = connect(port, Some("admin"), &opts);
    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.reply(PromptReply::Cancel);
    assert_eq!(
        c.event().await,
        SessionEvent::Disconnected {
            reason: Some("canceled".into())
        }
    );
}

#[tokio::test]
async fn unreachable_host_reports_an_error() {
    // Bind then drop a listener so the port is very likely closed.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let mut c = connect(port, Some("admin"), &options(temp_known_hosts()));
    match c.event().await {
        SessionEvent::Disconnected { reason: Some(r) } => assert!(!r.is_empty()),
        other => panic!("expected a connection error, got {other:?}"),
    }
}

async fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn echoes(port: u16) {
    let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    c.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut buf))
        .await
        .expect("no echo through the tunnel")
        .unwrap();
    assert_eq!(&buf, b"ping");
}

#[tokio::test]
async fn port_forwards_local_remote_and_socks() {
    let server = start_server(Mode::Password).await;
    let echo = echo_server().await;
    let (local, remote, socks) = (free_port(), free_port(), free_port());
    let fwd = |kind, bind_port, port| PortForward {
        kind,
        bind_address: String::new(),
        bind_port,
        host: "127.0.0.1".into(),
        port,
    };
    let mut target = TerminalTarget::new("127.0.0.1", server);
    target.username = Some("admin".into());
    target.forwards = vec![
        fwd(ForwardKind::Local, local, echo),
        fwd(ForwardKind::Remote, remote, echo),
        fwd(ForwardKind::Dynamic, socks, 0),
        fwd(ForwardKind::Local, local, echo), // same port again: fails, others keep working
    ];
    let opts = options(temp_known_hosts());
    let (tx, events) = unbounded_channel();
    let sink: EventSink = Box::new(move |e| {
        let _ = tx.send(e);
    });
    let mut c = Probe {
        session: SshSession::connect(&target, &opts, sink).unwrap(),
        events,
        screen: String::new(),
    };
    assert!(matches!(c.event().await, SessionEvent::HostKey(_)));
    c.reply(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
    assert!(matches!(c.event().await, SessionEvent::Auth(_)));
    c.password("secret");
    assert_eq!(c.event().await, SessionEvent::Connected);
    let SessionEvent::Forwards(states) = c.event().await else {
        panic!("expected forward states");
    };
    let errors: Vec<_> = states.iter().map(|s| s.error.is_some()).collect();
    assert_eq!(errors, [false, false, false, true], "{states:?}");
    assert!(c.screen.contains("Forwarding L 127.0.0.1:"), "{}", c.screen);

    echoes(local).await; // this computer → server → echo
    echoes(remote).await; // server's port → back to this computer → echo

    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", socks)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.unwrap();
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend_from_slice(&echo.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0, "SOCKS connect through SSH");
    s.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");

    c.session.close().unwrap();
}
