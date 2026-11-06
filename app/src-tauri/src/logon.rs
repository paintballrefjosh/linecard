//! Logon actions: after a terminal session connects, wait for each step's
//! text and send its line, the way SecureCRT's logon scripts get a switch to
//! `enable` and `terminal length 0`. Runs on its own thread, reading the
//! session's output from the same tap scripts use.

use std::sync::Arc;
use std::time::{Duration, Instant};

use linecard_core_model::{Credential, LogonStep};
use linecard_script::tap::Read;
use linecard_script::Tap;

/// How long the session may take to finish logging in.
const CONNECT_WAIT: Duration = Duration::from_secs(120);
/// How long each step waits for its text.
const STEP_WAIT: Duration = Duration::from_secs(30);
/// Output kept while looking for a step's text.
const KEEP: usize = 64 * 1024;

/// Session output as plain text, with escape sequences (colors, cursor
/// moves, titles) taken out so a colored prompt still matches.
#[derive(Default)]
pub struct Matcher {
    text: String,
    esc: Esc,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    None,
    /// After ESC.
    Start,
    /// ESC [ … up to a final byte.
    Csi,
    /// ESC ] … up to BEL or ESC \.
    Osc,
    /// ESC inside an OSC (maybe the start of ESC \).
    OscEnd,
}

impl Matcher {
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut plain = Vec::with_capacity(bytes.len());
        for &b in bytes {
            self.esc = match (self.esc, b) {
                (Esc::None, 0x1b) => Esc::Start,
                (Esc::None, _) => {
                    plain.push(b);
                    Esc::None
                }
                (Esc::Start, b'[') => Esc::Csi,
                (Esc::Start, b']') => Esc::Osc,
                (Esc::Start, _) => Esc::None,
                (Esc::Csi, 0x40..=0x7e) => Esc::None,
                (Esc::Csi, _) => Esc::Csi,
                (Esc::Osc, 0x07) => Esc::None,
                (Esc::Osc, 0x1b) => Esc::OscEnd,
                (Esc::Osc, _) => Esc::Osc,
                (Esc::OscEnd, _) => Esc::None,
            };
        }
        self.text.push_str(&String::from_utf8_lossy(&plain));
        if self.text.len() > KEEP {
            let mut cut = self.text.len() - KEEP;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }

    /// Whether `needle` has appeared; if so, everything up to it is used up,
    /// so the next step only matches what comes after.
    pub fn take(&mut self, needle: &str) -> bool {
        match self.text.find(needle) {
            Some(at) => {
                self.text.drain(..at + needle.len());
                true
            }
            None => false,
        }
    }
}

/// What the runner needs from the app.
pub trait Io: Send + 'static {
    /// Type into the session.
    fn write(&self, data: &[u8]) -> Result<(), String>;
    /// A saved password by keychain slot.
    fn password(&self, slot: &str) -> Option<String>;
    /// Show a dim note in the terminal.
    fn notice(&self, text: &str);
}

/// The app's side: the session by id, saved passwords, and the tab's channel.
pub struct AppIo {
    pub app: tauri::AppHandle,
    pub session: String,
    pub channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
}

impl Io for AppIo {
    fn write(&self, data: &[u8]) -> Result<(), String> {
        use tauri::Manager;
        let state = self.app.state::<crate::AppState>();
        let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        let s = sessions.get_mut(&self.session).ok_or("that session is closed")?;
        s.write(data).map_err(|e| e.to_string())
    }
    fn password(&self, slot: &str) -> Option<String> {
        use tauri::Manager;
        let state = self.app.state::<crate::AppState>();
        state.ssh.credentials.as_ref()?.password(slot)
    }
    fn notice(&self, text: &str) {
        let _ = self.channel.send(tauri::ipc::InvokeResponseBody::Raw(
            format!("\r\n\x1b[2m[{text}]\x1b[0m\r\n").into_bytes(),
        ));
    }
}

/// Run `steps` against the session in the background.
pub fn spawn(tap: Arc<Tap>, steps: Vec<LogonStep>, io: impl Io) {
    if steps.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("logon".into())
        .spawn(move || run(&tap, &steps, &io));
}

fn run(tap: &Tap, steps: &[LogonStep], io: &impl Io) {
    if tap.wait_connected(CONNECT_WAIT).is_err() {
        return;
    }
    let mut cursor = 0;
    let mut m = Matcher::default();
    for step in steps {
        if !step.expect.is_empty() {
            let deadline = Instant::now() + STEP_WAIT;
            while !m.take(&step.expect) {
                let left = deadline.saturating_duration_since(Instant::now());
                match tap.read(&mut cursor, left) {
                    Read::Data { bytes, .. } => m.feed(&bytes),
                    Read::Timeout => {
                        io.notice(&format!(
                            "Logon actions stopped: \"{}\" didn't appear within {} seconds",
                            step.expect,
                            STEP_WAIT.as_secs()
                        ));
                        return;
                    }
                    Read::Ended(_) => return,
                }
            }
        }
        let line = match &step.password_of {
            Some(id) => match io.password(&Credential::slot(id)) {
                Some(p) => p,
                None => {
                    io.notice("Logon actions stopped: no password is saved for that credential on this computer");
                    return;
                }
            },
            None => step.send.clone(),
        };
        if io.write(format!("{line}\r").as_bytes()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn colors_and_titles_are_ignored_and_matches_are_used_up() {
        let mut m = Matcher::default();
        m.feed(b"\x1b]0;sw1\x07Welcome\r\n\x1b[1;32msw1\x1b[0m>");
        assert!(m.take("sw1>"));
        assert!(!m.take("sw1>"), "a prompt only counts once");
        // An escape sequence split across reads.
        m.feed(b"Pass\x1b[");
        m.feed(b"0mword:");
        assert!(m.take("Password:"));
    }

    #[derive(Default)]
    struct Fake {
        sent: Mutex<Vec<String>>,
        notes: Mutex<Vec<String>>,
    }
    impl Io for Arc<Fake> {
        fn write(&self, data: &[u8]) -> Result<(), String> {
            self.sent.lock().unwrap().push(String::from_utf8_lossy(data).into());
            Ok(())
        }
        fn password(&self, slot: &str) -> Option<String> {
            (slot == "cred:en").then(|| "s3cret".into())
        }
        fn notice(&self, text: &str) {
            self.notes.lock().unwrap().push(text.into());
        }
    }

    fn step(expect: &str, send: &str, password_of: Option<&str>) -> LogonStep {
        LogonStep {
            expect: expect.into(),
            send: send.into(),
            password_of: password_of.map(Into::into),
        }
    }

    #[test]
    fn steps_wait_for_their_prompt_then_send() {
        let tap = Arc::new(Tap::default());
        let fake = Arc::new(Fake::default());
        tap.push(b"banner\r\nsw1>");
        tap.set_connected();
        let feeder = {
            let tap = tap.clone();
            let fake = fake.clone();
            std::thread::spawn(move || {
                // Answer each line as a switch would.
                let wait_for = |n: usize| {
                    while fake.sent.lock().unwrap().len() < n {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                };
                wait_for(1);
                tap.push(b"enable\r\nPassword: ");
                wait_for(2);
                tap.push(b"\r\nsw1#");
                wait_for(3);
                tap.end(None);
            })
        };
        run(
            &tap,
            &[
                step(">", "enable", None),
                step("Password:", "", Some("en")),
                step("#", "terminal length 0", None),
            ],
            &fake,
        );
        feeder.join().unwrap();
        assert_eq!(
            *fake.sent.lock().unwrap(),
            ["enable\r", "s3cret\r", "terminal length 0\r"]
        );
        assert!(fake.notes.lock().unwrap().is_empty());
    }

    #[test]
    fn a_missing_password_stops_with_a_note() {
        let tap = Arc::new(Tap::default());
        let fake = Arc::new(Fake::default());
        tap.set_connected();
        run(&tap, &[step("", "", Some("nope")), step("", "never", None)], &fake);
        assert!(fake.sent.lock().unwrap().is_empty());
        assert_eq!(fake.notes.lock().unwrap().len(), 1);
    }
}
