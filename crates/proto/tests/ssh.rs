//! End-to-end tests for the SSH driver against an in-process russh server
//! on 127.0.0.1: host-key prompts and known_hosts, password and
//! keyboard-interactive auth, shell I/O, resize, cancel and close.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linecard_proto::ssh::SshSession;
use linecard_proto::{
    Credentials, EventSink, HostKeyDecision, HostKeyStatus, PromptReply, Session, SessionEvent, SshOptions,
    TerminalTarget,
};
use russh::keys::PrivateKey;
use russh::server::{self, Auth, Msg, Response};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
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

// ---- test server ---------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Mode {
    Password,
    KeyboardInteractive,
}

struct Shell;

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

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
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
        session.data(channel, data.to_ascii_uppercase())?;
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
    let methods: &[MethodKind] = match mode {
        Mode::Password => &[MethodKind::Password],
        Mode::KeyboardInteractive => &[MethodKind::KeyboardInteractive],
    };
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from_openssh(HOST_KEY).unwrap()],
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::from_millis(5),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(running) = server::run_stream(config, stream, Shell).await {
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
