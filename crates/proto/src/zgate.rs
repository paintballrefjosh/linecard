//! ZMODEM for terminal sessions: watches a session's output for a remote
//! `sz` or `rz` starting, and runs the transfer the user agrees to.
//!
//! Wrap the event sink before connecting ([`Gate::new`]) and the session
//! after ([`Gate::wrap`]). When the start of a transfer shows up in the
//! output, everything from there on goes to the transfer instead of the
//! terminal, and the window is told with [`SessionEvent::Zmodem`]. It then
//! answers with [`TransferRequest::Zmodem`]: a file to send, a folder to
//! receive into, or nothing to decline. Typing is held back while a
//! transfer is offered or running, so it can't corrupt the stream.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::xmodem::Link;
use crate::zmodem::{self, Options, ABORT, RECEIVER_START, SENDER_START};
use crate::{ConnectionKind, EventSink, PromptReply, Session, SessionEvent, TransferRequest, TransferStatus};

/// An offer nobody answers is declined after this, so the terminal can't
/// stay frozen (lrzsz gives up on its own after about a minute too).
const OFFER_WAIT: Duration = Duration::from_secs(90);

/// The wrapped session, shared with the transfer thread that writes to it.
type Inner = Arc<Mutex<Box<dyn Session>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where a remote transfer stands.
enum Phase {
    /// Output goes to the terminal.
    Idle,
    /// A transfer started on the server; waiting for the user. Output since
    /// then is kept here for the transfer.
    Offered {
        upload: bool,
        rx: Receiver<Vec<u8>>,
        /// Tells the offer's timer it was answered.
        id: u64,
    },
    /// Running; output goes to it.
    Running { cancel: Arc<AtomicBool> },
}

struct Shared {
    events: Mutex<EventSink>,
    /// While a transfer is offered or running, output goes here.
    route: Mutex<Option<Sender<Vec<u8>>>>,
    phase: Mutex<Phase>,
    /// The last few bytes shown, so a start split across reads is still seen.
    tail: Mutex<Vec<u8>>,
    /// The session, once connected (the transfer writes through it).
    inner: Mutex<Option<Inner>>,
    opts: Options,
    offers: AtomicU64,
}

impl Shared {
    fn emit(&self, e: SessionEvent) {
        (lock(&self.events))(e);
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        let inner = lock(&self.inner)
            .clone()
            .ok_or_else(|| io::Error::other("not connected"))?;
        let mut s = lock(&inner);
        s.write(data)
    }

    /// Output from the session.
    fn output(self: &Arc<Self>, data: Vec<u8>) {
        if let Some(tx) = lock(&self.route).as_ref() {
            let _ = tx.send(data);
            return;
        }
        let mut tail = lock(&self.tail);
        let mut seen = std::mem::take(&mut *tail);
        let shown = seen.len();
        seen.extend_from_slice(&data);
        let start = [(SENDER_START, false), (RECEIVER_START, true)]
            .iter()
            .filter_map(|(pat, upload)| find(&seen, pat).map(|at| (at, *upload)))
            .min_by_key(|(at, _)| *at);
        let Some((at, upload)) = start else {
            let keep = seen.len().saturating_sub(SENDER_START.len() - 1);
            *tail = seen[keep..].to_vec();
            drop(tail);
            self.emit(SessionEvent::Data(data));
            return;
        };
        drop(tail);
        // Show what came before it; the rest belongs to the transfer.
        let before = at.saturating_sub(shown);
        if before > 0 {
            self.emit(SessionEvent::Data(data[..before].to_vec()));
        }
        let (tx, rx) = channel();
        let _ = tx.send(seen[at..].to_vec());
        *lock(&self.route) = Some(tx);
        let id = self.offers.fetch_add(1, Ordering::Relaxed) + 1;
        *lock(&self.phase) = Phase::Offered { upload, rx, id };
        self.emit(SessionEvent::Zmodem { upload });
        // Decline it if nobody answers.
        let me = Arc::clone(self);
        let _ = thread::Builder::new().name("zmodem-offer".into()).spawn(move || {
            thread::sleep(OFFER_WAIT);
            let still = matches!(&*lock(&me.phase), Phase::Offered { id: i, .. } if *i == id);
            if still {
                me.decline();
            }
        });
    }

    /// Say no to an offered transfer: stop the remote program and give the
    /// terminal back.
    fn decline(&self) {
        let was = std::mem::replace(&mut *lock(&self.phase), Phase::Idle);
        if !matches!(was, Phase::Offered { .. }) {
            *lock(&self.phase) = was;
            return;
        }
        let _ = self.write(ABORT);
        // Let the remote program print its farewell (and the shell its
        // prompt) before output goes back to the terminal, minus the
        // transfer's own noise.
        thread::sleep(Duration::from_millis(300));
        *lock(&self.route) = None;
        lock(&self.tail).clear();
    }

    fn start(self: &Arc<Self>, path: Option<PathBuf>) -> io::Result<()> {
        let mut phase = lock(&self.phase);
        let (upload, rx, id) = match std::mem::replace(&mut *phase, Phase::Idle) {
            Phase::Offered { upload, rx, id } => (upload, rx, id),
            other => {
                *phase = other;
                return Err(io::Error::other("no ZMODEM transfer is waiting"));
            }
        };
        let Some(path) = path else {
            *phase = Phase::Offered { upload, rx, id };
            drop(phase);
            self.decline();
            return Ok(());
        };
        // Open the file (or check the folder) first so a mistake shows now.
        let file = if upload {
            Some(std::fs::File::open(&path)?)
        } else {
            if !path.is_dir() {
                *phase = Phase::Offered { upload, rx, id };
                return Err(io::Error::new(io::ErrorKind::NotFound, "pick a folder to save into"));
            }
            None
        };
        let cancel = Arc::new(AtomicBool::new(false));
        *phase = Phase::Running { cancel: cancel.clone() };
        drop(phase);

        let me = Arc::clone(self);
        thread::Builder::new().name("zmodem".into()).spawn(move || {
            let mut link = GateLink {
                shared: &me,
                rx,
                held: VecDeque::new(),
            };
            let mut shown = String::new();
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut progress = |name: &str, done: u64, total: Option<u64>| {
                if name != shown || last.elapsed() >= Duration::from_millis(100) {
                    shown = name.to_string();
                    last = Instant::now();
                    me.emit(SessionEvent::Transfer(TransferStatus {
                        file: name.to_string(),
                        sending: upload,
                        done,
                        total,
                        result: None,
                    }));
                }
            };
            let (name, result) = match file {
                Some(mut f) => {
                    let name = path
                        .file_name()
                        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
                    let r = zmodem::send(&mut link, &mut f, &name, me.opts, &cancel, &mut progress);
                    (name, r)
                }
                None => {
                    let r = zmodem::receive(&mut link, &path, me.opts, &cancel, &mut progress);
                    let r = r.map(|files| {
                        files
                            .iter()
                            .filter_map(|p| std::fs::metadata(p).ok())
                            .map(|m| m.len())
                            .sum::<u64>()
                    });
                    (path.display().to_string(), r)
                }
            };
            // Whatever the server printed after the transfer (its prompt)
            // goes to the terminal.
            thread::sleep(Duration::from_millis(200));
            let mut rest: Vec<u8> = link.held.drain(..).collect();
            while let Ok(chunk) = link.rx.try_recv() {
                rest.extend(chunk);
            }
            *lock(&me.route) = None;
            *lock(&me.phase) = Phase::Idle;
            lock(&me.tail).clear();
            let result = result.map_err(|e| e.to_string());
            me.emit(SessionEvent::Transfer(TransferStatus {
                file: name,
                sending: upload,
                done: *result.as_ref().unwrap_or(&0),
                total: None,
                result: Some(result.map(|_| ())),
            }));
            // Drop the transfer's trailing noise ("OO", line ends) but keep text.
            let text: Vec<u8> = trim_noise(&rest);
            if !text.is_empty() {
                me.emit(SessionEvent::Data(text));
            }
        })?;
        Ok(())
    }
}

/// What's left after a transfer, without the protocol's own bytes before the
/// first printable line.
fn trim_noise(rest: &[u8]) -> Vec<u8> {
    let start = rest
        .iter()
        .position(|&b| b != b'O' && b != b'\r' && b != b'\n' && b != 0x8a && b != 0x11 && b != 0x18 && b != 8)
        .unwrap_or(rest.len());
    rest[start..].to_vec()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The session as a ZMODEM line: writes go through the session, reads come
/// from the routed output.
struct GateLink<'a> {
    shared: &'a Shared,
    rx: Receiver<Vec<u8>>,
    held: VecDeque<u8>,
}

impl Link for GateLink<'_> {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.shared.write(data)
    }
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        if self.held.is_empty() {
            if let Ok(chunk) = self.rx.recv_timeout(timeout) {
                self.held.extend(chunk);
            }
        }
        Ok(self.held.pop_front())
    }
    fn pending(&mut self) -> io::Result<bool> {
        while let Ok(chunk) = self.rx.try_recv() {
            self.held.extend(chunk);
        }
        Ok(!self.held.is_empty())
    }
}

/// Watches one session for ZMODEM.
pub struct Gate {
    shared: Arc<Shared>,
}

impl Gate {
    /// Wrap `events`; pass the returned sink to the driver. `escape_ctl` for
    /// lines that aren't 8-bit clean (Telnet).
    pub fn new(events: EventSink, escape_ctl: bool) -> (Gate, EventSink) {
        let shared = Arc::new(Shared {
            events: Mutex::new(events),
            route: Mutex::new(None),
            phase: Mutex::new(Phase::Idle),
            tail: Mutex::new(Vec::new()),
            inner: Mutex::new(None),
            opts: Options { escape_ctl },
            offers: AtomicU64::new(0),
        });
        let s = Arc::clone(&shared);
        let sink: EventSink = Box::new(move |e| match e {
            SessionEvent::Data(d) => s.output(d),
            other => s.emit(other),
        });
        (Gate { shared }, sink)
    }

    /// The connected session, with ZMODEM handled.
    pub fn wrap(self, session: Box<dyn Session>) -> Box<dyn Session> {
        let inner = Arc::new(Mutex::new(session));
        *lock(&self.shared.inner) = Some(Arc::clone(&inner));
        Box::new(Gated {
            inner,
            shared: self.shared,
        })
    }
}

struct Gated {
    inner: Inner,
    shared: Arc<Shared>,
}

impl Gated {
    fn busy(&self) -> bool {
        !matches!(&*lock(&self.shared.phase), Phase::Idle)
    }
}

impl Session for Gated {
    fn kind(&self) -> ConnectionKind {
        lock(&self.inner).kind()
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if self.busy() {
            // Keystrokes would land in the middle of the transfer.
            return Ok(());
        }
        lock(&self.inner).write(data)
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        lock(&self.inner).resize(cols, rows)
    }

    fn reply(&mut self, reply: PromptReply) -> io::Result<()> {
        lock(&self.inner).reply(reply)
    }

    fn start_transfer(&mut self, req: TransferRequest) -> io::Result<()> {
        match req {
            TransferRequest::Zmodem { path } => self.shared.start(path),
            other => lock(&self.inner).start_transfer(other),
        }
    }

    fn cancel_transfer(&mut self) {
        let running = match &*lock(&self.shared.phase) {
            Phase::Running { cancel } => {
                cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        };
        if !running {
            self.shared.decline();
            lock(&self.inner).cancel_transfer();
        }
    }

    fn send_break(&mut self) -> io::Result<()> {
        lock(&self.inner).send_break()
    }

    fn close(&mut self) -> io::Result<()> {
        if let Phase::Running { cancel } = &*lock(&self.shared.phase) {
            cancel.store(true, Ordering::Relaxed);
        }
        lock(&self.inner).close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver as Rx;

    /// A session whose output we feed by hand and whose input we collect.
    struct Echo {
        written: Arc<Mutex<Vec<u8>>>,
    }
    impl Session for Echo {
        fn kind(&self) -> ConnectionKind {
            ConnectionKind::Ssh
        }
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            lock(&self.written).extend_from_slice(data);
            Ok(())
        }
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> {
            Ok(())
        }
        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    type Setup = (Box<dyn Session>, EventSink, Rx<SessionEvent>, Arc<Mutex<Vec<u8>>>);

    fn setup() -> Setup {
        let (tx, rx) = channel();
        let (gate, mut sink) = Gate::new(
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
            false,
        );
        let written = Arc::new(Mutex::new(Vec::new()));
        let session = gate.wrap(Box::new(Echo {
            written: written.clone(),
        }));
        sink(SessionEvent::Connected);
        (session, sink, rx, written)
    }

    fn drain(rx: &Rx<SessionEvent>) -> Vec<SessionEvent> {
        let mut out = vec![];
        while let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
            out.push(e);
        }
        out
    }

    #[test]
    fn a_start_split_across_reads_is_offered_and_hides_the_rest() {
        let (mut session, mut sink, rx, written) = setup();
        sink(SessionEvent::Data(b"$ rz\r\nrz waiting to receive.**".to_vec()));
        sink(SessionEvent::Data(b"\x18B0100000023be50\r\x8a\x11".to_vec()));
        sink(SessionEvent::Data(b"more protocol".to_vec()));
        let events = drain(&rx);
        assert!(matches!(&events[0], SessionEvent::Connected));
        let shown: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Data(d) => Some(d.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        // The "**" before the split was already on screen; nothing after it is.
        assert_eq!(shown, b"$ rz\r\nrz waiting to receive.**");
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::Zmodem { upload: true })));

        // Typing is held back while it's offered.
        session.write(b"ls\r").unwrap();
        assert!(lock(&written).is_empty());

        // Declining stops rz and gives the terminal back.
        session.cancel_transfer();
        assert!(lock(&written).starts_with(&[ZDLE_CAN; 5]));
        sink(SessionEvent::Data(b"$ ".to_vec()));
        session.write(b"ls\r").unwrap();
        assert!(lock(&written).ends_with(b"ls\r"));
        let after = drain(&rx);
        assert!(after.iter().any(|e| matches!(e, SessionEvent::Data(d) if d == b"$ ")));
    }

    const ZDLE_CAN: u8 = 0x18;

    #[test]
    fn plain_output_passes_through() {
        let (_session, mut sink, rx, _) = setup();
        sink(SessionEvent::Data(b"show version\r\n**not zmodem**\r\n".to_vec()));
        let events = drain(&rx);
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::Data(d) if d == b"show version\r\n**not zmodem**\r\n")));
        assert!(!events.iter().any(|e| matches!(e, SessionEvent::Zmodem { .. })));
    }

    #[test]
    fn answering_with_no_offer_is_an_error() {
        let (mut session, _sink, _rx, _) = setup();
        assert!(session
            .start_transfer(TransferRequest::Zmodem {
                path: Some(PathBuf::from("."))
            })
            .is_err());
    }

    /// A session whose input goes down a channel (to the fake server).
    struct Wire(Sender<Vec<u8>>);
    impl Session for Wire {
        fn kind(&self) -> ConnectionKind {
            ConnectionKind::Ssh
        }
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            let _ = self.0.send(data.to_vec());
            Ok(())
        }
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> {
            Ok(())
        }
        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The server's end: what it sends arrives as session output.
    struct Server {
        sink: Arc<Mutex<EventSink>>,
        rx: Rx<Vec<u8>>,
        held: VecDeque<u8>,
    }
    impl Link for Server {
        fn send(&mut self, data: &[u8]) -> io::Result<()> {
            (lock(&self.sink))(SessionEvent::Data(data.to_vec()));
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
            if self.held.is_empty() {
                if let Ok(c) = self.rx.recv_timeout(timeout) {
                    self.held.extend(c);
                }
            }
            Ok(self.held.pop_front())
        }
        fn pending(&mut self) -> io::Result<bool> {
            while let Ok(c) = self.rx.try_recv() {
                self.held.extend(c);
            }
            Ok(!self.held.is_empty())
        }
    }

    #[test]
    fn a_file_from_sz_is_saved_where_the_user_says() {
        for escape_ctl in [false, true] {
            let (ev_tx, ev_rx) = channel();
            let (gate, sink) = Gate::new(
                Box::new(move |e| {
                    let _ = ev_tx.send(e);
                }),
                escape_ctl,
            );
            let (to_server, from_client) = channel();
            let mut session = gate.wrap(Box::new(Wire(to_server)));
            let sink = Arc::new(Mutex::new(sink));

            let dir = tempfile::tempdir().unwrap();
            let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + i / 255) as u8).collect();
            let src = dir.path().join("running-config");
            std::fs::write(&src, &data).unwrap();
            let into = dir.path().join("saved");
            std::fs::create_dir(&into).unwrap();

            // The "server" runs sz: a banner, then the transfer, then a prompt.
            let server_sink = sink.clone();
            let server = thread::spawn(move || {
                (lock(&server_sink))(SessionEvent::Data(b"$ sz running-config\r\n".to_vec()));
                let mut link = Server {
                    sink: server_sink.clone(),
                    rx: from_client,
                    held: VecDeque::new(),
                };
                link.send(b"**\x18B00000000000000\r\x8a\x11").unwrap();
                let mut f = std::fs::File::open(&src).unwrap();
                let cancel = AtomicBool::new(false);
                let n = zmodem::send(
                    &mut link,
                    &mut f,
                    "running-config",
                    Options { escape_ctl },
                    &cancel,
                    &mut |_, _, _| {},
                )
                .unwrap();
                (lock(&server_sink))(SessionEvent::Data(b"$ ".to_vec()));
                n
            });

            // The window is asked, and answers with a folder.
            let offered = loop {
                if let SessionEvent::Zmodem { upload } = ev_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    break upload;
                }
            };
            assert!(!offered, "sz offers files to receive");
            session
                .start_transfer(TransferRequest::Zmodem {
                    path: Some(into.clone()),
                })
                .unwrap();
            assert_eq!(server.join().unwrap(), data.len() as u64);

            let mut finished = None;
            let mut shown = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && (finished.is_none() || shown.is_empty()) {
                match ev_rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(SessionEvent::Transfer(t)) if t.result.is_some() => finished = t.result,
                    Ok(SessionEvent::Data(d)) => shown.extend(d),
                    _ => {}
                }
            }
            assert_eq!(finished, Some(Ok(())));
            let got = std::fs::read(into.join("running-config")).unwrap();
            let first = got.iter().zip(&data).position(|(a, b)| a != b);
            assert!(
                got == data,
                "escape_ctl {escape_ctl}: got {} of {}, first difference at {first:?}",
                got.len(),
                data.len()
            );
            // The prompt after the transfer reaches the terminal.
            assert!(shown.ends_with(b"$ "), "{:?}", String::from_utf8_lossy(&shown));
        }
    }

    #[test]
    fn trailing_protocol_bytes_are_dropped() {
        assert_eq!(trim_noise(b"OO\r\n\x8a$ "), b"$ ");
        assert_eq!(trim_noise(b"OO"), b"");
    }
}
