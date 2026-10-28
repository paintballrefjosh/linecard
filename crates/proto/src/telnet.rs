//! Telnet (RFC 854) with the options network gear and console servers use:
//! ECHO (857), SUPPRESS-GO-AHEAD (858), TERMINAL-TYPE (1091), NAWS (1073)
//! and BINARY (856).
//!
//! [`TelnetCodec`] is a pure byte-level state machine (easy to test);
//! [`TelnetSession`] wraps it around a TCP socket and a reader thread.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::{ConnectionKind, EventSink, Session, SessionEvent, TerminalTarget};

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const SE: u8 = 240;

pub const OPT_BINARY: u8 = 0;
pub const OPT_ECHO: u8 = 1;
pub const OPT_SGA: u8 = 3;
pub const OPT_TTYPE: u8 = 24;
pub const OPT_NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// Options we agree to perform ourselves (answer DO with WILL).
const LOCAL_OK: &[u8] = &[OPT_BINARY, OPT_SGA, OPT_TTYPE, OPT_NAWS];
/// Options we accept from the server (answer WILL with DO).
const REMOTE_OK: &[u8] = &[OPT_BINARY, OPT_ECHO, OPT_SGA];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    Negotiate(u8),
    Sub,
    SubIac,
}

/// Result of feeding received bytes through the codec.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    /// Bytes for the terminal.
    pub data: Vec<u8>,
    /// Negotiation replies to send back to the server.
    pub reply: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TelnetCodec {
    state: State,
    sub: Vec<u8>,
    term_type: String,
    cols: u16,
    rows: u16,
    local: BTreeSet<u8>,
    remote: BTreeSet<u8>,
}

impl TelnetCodec {
    pub fn new(term_type: impl Into<String>, cols: u16, rows: u16) -> Self {
        TelnetCodec {
            state: State::Data,
            sub: Vec::new(),
            term_type: term_type.into(),
            cols,
            rows,
            local: BTreeSet::new(),
            remote: BTreeSet::new(),
        }
    }

    /// Server is echoing our input (so the terminal must not echo locally).
    pub fn remote_echo(&self) -> bool {
        self.remote.contains(&OPT_ECHO)
    }

    pub fn feed(&mut self, input: &[u8]) -> Decoded {
        let mut out = Decoded::default();
        for &b in input {
            self.state = match (self.state, b) {
                (State::Data, IAC) => State::Iac,
                (State::Data, b) => {
                    out.data.push(b);
                    State::Data
                }
                (State::Iac, IAC) => {
                    out.data.push(IAC);
                    State::Data
                }
                (State::Iac, DO | DONT | WILL | WONT) => State::Negotiate(b),
                (State::Iac, SB) => {
                    self.sub.clear();
                    State::Sub
                }
                // NOP, GA, AYT, etc.: nothing to do.
                (State::Iac, _) => State::Data,
                (State::Negotiate(cmd), opt) => {
                    self.negotiate(cmd, opt, &mut out.reply);
                    State::Data
                }
                (State::Sub, IAC) => State::SubIac,
                (State::Sub, b) => {
                    self.sub.push(b);
                    State::Sub
                }
                (State::SubIac, SE) => {
                    self.subnegotiation(&mut out.reply);
                    State::Data
                }
                (State::SubIac, IAC) => {
                    self.sub.push(IAC);
                    State::Sub
                }
                // Malformed; resynchronise.
                (State::SubIac, _) => State::Data,
            };
        }
        out
    }

    fn negotiate(&mut self, cmd: u8, opt: u8, reply: &mut Vec<u8>) {
        match cmd {
            DO if LOCAL_OK.contains(&opt) => {
                if self.local.insert(opt) {
                    reply.extend([IAC, WILL, opt]);
                    if opt == OPT_NAWS {
                        reply.extend(self.naws());
                    }
                }
            }
            DO => reply.extend([IAC, WONT, opt]),
            DONT => {
                if self.local.remove(&opt) {
                    reply.extend([IAC, WONT, opt]);
                }
            }
            WILL if REMOTE_OK.contains(&opt) => {
                if self.remote.insert(opt) {
                    reply.extend([IAC, DO, opt]);
                }
            }
            WILL => reply.extend([IAC, DONT, opt]),
            WONT => {
                if self.remote.remove(&opt) {
                    reply.extend([IAC, DONT, opt]);
                }
            }
            _ => {}
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sub.as_slice() == [OPT_TTYPE, TTYPE_SEND] && self.local.contains(&OPT_TTYPE) {
            reply.extend([IAC, SB, OPT_TTYPE, TTYPE_IS]);
            reply.extend(self.term_type.bytes());
            reply.extend([IAC, SE]);
        }
    }

    fn naws(&self) -> Vec<u8> {
        let mut v = vec![IAC, SB, OPT_NAWS];
        for byte in self.cols.to_be_bytes().into_iter().chain(self.rows.to_be_bytes()) {
            v.push(byte);
            if byte == IAC {
                v.push(IAC);
            }
        }
        v.extend([IAC, SE]);
        v
    }

    /// New window size. Returns the NAWS message to send, if NAWS is active.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        self.cols = cols;
        self.rows = rows;
        self.local.contains(&OPT_NAWS).then(|| self.naws())
    }

    /// Prepare user input for the wire: escape IAC and, outside binary mode,
    /// send a bare CR as CR NUL as RFC 854 requires.
    pub fn encode(&self, input: &[u8]) -> Vec<u8> {
        let binary = self.local.contains(&OPT_BINARY);
        let mut out = Vec::with_capacity(input.len() + 4);
        for (i, &b) in input.iter().enumerate() {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            } else if b == b'\r' && !binary && input.get(i + 1) != Some(&b'\n') {
                out.push(0);
            }
        }
        out
    }
}

/// A Telnet session over TCP.
pub struct TelnetSession {
    stream: TcpStream,
    codec: Arc<Mutex<TelnetCodec>>,
}

impl TelnetSession {
    pub fn connect(target: &TerminalTarget, mut events: EventSink) -> io::Result<Self> {
        let addr = (target.host.as_str(), target.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "host did not resolve"))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
        stream.set_nodelay(true)?;
        let codec = Arc::new(Mutex::new(TelnetCodec::new(
            &target.term_type,
            target.cols,
            target.rows,
        )));

        let mut reader = stream.try_clone()?;
        let mut replier = stream.try_clone()?;
        let rx_codec = Arc::clone(&codec);
        events(SessionEvent::Connected);
        thread::Builder::new().name("telnet-rx".into()).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let reason = loop {
                match reader.read(&mut buf) {
                    Ok(0) => break None,
                    Ok(n) => {
                        let decoded = rx_codec.lock().unwrap().feed(&buf[..n]);
                        if !decoded.reply.is_empty() {
                            if let Err(e) = replier.write_all(&decoded.reply) {
                                break Some(e.to_string());
                            }
                        }
                        if !decoded.data.is_empty() {
                            events(SessionEvent::Data(decoded.data));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => break Some(e.to_string()),
                }
            };
            events(SessionEvent::Disconnected { reason });
        })?;
        Ok(TelnetSession { stream, codec })
    }
}

impl Session for TelnetSession {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Telnet
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let bytes = self.codec.lock().unwrap().encode(data);
        self.stream.write_all(&bytes)
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let msg = self.codec.lock().unwrap().resize(cols, rows);
        match msg {
            Some(m) => self.stream.write_all(&m),
            None => Ok(()),
        }
    }

    fn close(&mut self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn negotiates_common_options() {
        let mut c = TelnetCodec::new("xterm-256color", 132, 40);
        let d = c.feed(&[IAC, DO, OPT_NAWS, IAC, WILL, OPT_ECHO, IAC, DO, 99, b'h', b'i']);
        assert_eq!(d.data, b"hi");
        assert_eq!(
            d.reply,
            vec![IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 132, 0, 40, IAC, SE, IAC, DO, OPT_ECHO, IAC, WONT, 99]
        );
        assert!(c.remote_echo());
        // Repeating a request we already agreed to must not trigger a reply loop.
        assert!(c.feed(&[IAC, WILL, OPT_ECHO]).reply.is_empty());
    }

    #[test]
    fn terminal_type_subnegotiation() {
        let mut c = TelnetCodec::new("vt100", 80, 24);
        c.feed(&[IAC, DO, OPT_TTYPE]);
        let d = c.feed(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expect = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expect.extend(b"vt100");
        expect.extend([IAC, SE]);
        assert_eq!(d.reply, expect);
    }

    #[test]
    fn escapes_and_split_sequences() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        // IAC IAC is a literal 255; a command split across reads still parses.
        assert_eq!(c.feed(&[b'a', IAC]).data, b"a");
        assert_eq!(c.feed(&[IAC, b'b']).data, vec![IAC, b'b']);
        assert_eq!(c.feed(&[IAC]).reply, Vec::<u8>::new());
        assert_eq!(c.feed(&[WILL]).reply, Vec::<u8>::new());
        assert_eq!(c.feed(&[OPT_SGA]).reply, vec![IAC, DO, OPT_SGA]);
        // Outgoing: IAC doubled, bare CR becomes CR NUL, CRLF untouched.
        assert_eq!(c.encode(&[b'x', IAC, b'\r']), vec![b'x', IAC, IAC, b'\r', 0]);
        assert_eq!(c.encode(b"ok\r\n"), b"ok\r\n");
    }

    #[test]
    fn naws_doubles_iac_bytes_and_resize_needs_naws() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        assert_eq!(c.resize(100, 30), None);
        c.feed(&[IAC, DO, OPT_NAWS]);
        assert_eq!(
            c.resize(255, 50).unwrap(),
            vec![IAC, SB, OPT_NAWS, 0, IAC, IAC, 0, 50, IAC, SE]
        );
    }

    #[test]
    fn session_talks_to_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(&[IAC, WILL, OPT_ECHO, IAC, DO, OPT_NAWS]).unwrap();
            s.write_all(b"Router login: ").unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.ends_with(b"admin\r\0") {
                let n = s.read(&mut buf).unwrap();
                assert!(n > 0, "client closed early");
                got.extend_from_slice(&buf[..n]);
            }
            got
        });

        let (tx, rx) = mpsc::channel();
        let mut session = connect_test(port, tx);
        let mut screen = Vec::new();
        while !screen.ends_with(b"login: ") {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                SessionEvent::Data(d) => screen.extend(d),
                _ => {}
            }
        }
        session.write(b"admin\r").unwrap();
        let got = server.join().unwrap();
        assert!(got.windows(3).any(|w| w == [IAC, DO, OPT_ECHO]));
        assert!(got.windows(3).any(|w| w == [IAC, WILL, OPT_NAWS]));
        assert!(got.windows(7).any(|w| w == [IAC, SB, OPT_NAWS, 0, 80, 0, 24]));
        session.close().unwrap();
    }

    fn connect_test(port: u16, tx: mpsc::Sender<SessionEvent>) -> TelnetSession {
        let target = TerminalTarget::new("127.0.0.1", port);
        TelnetSession::connect(
            &target,
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .unwrap()
    }
}
