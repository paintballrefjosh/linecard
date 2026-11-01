//! What a session printed lately, kept so a script can wait for it.
//!
//! Every terminal session feeds a [`Tap`]. A script reads from its own
//! position (a cursor) in the stream, so it sees output that arrives after it
//! started looking, however long it spends between calls.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How much output is kept per session. A script that falls further behind
/// than this is told it missed some.
pub const CAPACITY: usize = 512 * 1024;

#[derive(Default)]
struct State {
    buf: VecDeque<u8>,
    /// Stream offset of `buf[0]`; `start + buf.len()` is the total so far.
    start: u64,
    connected: bool,
    /// `Some(reason)` once the session is over (`reason` is `None` for a clean close).
    ended: Option<Option<String>>,
}

#[derive(Default)]
pub struct Tap {
    state: Mutex<State>,
    changed: Condvar,
}

/// The result of waiting on a tap.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// New output. `lost` means older output was dropped before it was read.
    Data { bytes: Vec<u8>, lost: bool },
    /// Nothing arrived in time.
    Timeout,
    /// The session is over and everything it printed has been read.
    Ended(Option<String>),
}

/// Why waiting for a connection stopped without one.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    Timeout,
    Ended(Option<String>),
}

impl Tap {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Output from the session.
    pub fn push(&self, data: &[u8]) {
        let mut s = self.lock();
        s.buf.extend(data);
        if s.buf.len() > CAPACITY {
            let drop = s.buf.len() - CAPACITY;
            s.buf.drain(..drop);
            s.start += drop as u64;
        }
        drop(s);
        self.changed.notify_all();
    }

    /// The session is up (login finished).
    pub fn set_connected(&self) {
        self.lock().connected = true;
        self.changed.notify_all();
    }

    pub fn is_connected(&self) -> bool {
        let s = self.lock();
        s.connected && s.ended.is_none()
    }

    pub fn is_ended(&self) -> bool {
        self.lock().ended.is_some()
    }

    /// The session is over.
    pub fn end(&self, reason: Option<String>) {
        let mut s = self.lock();
        if s.ended.is_none() {
            s.ended = Some(reason);
        }
        drop(s);
        self.changed.notify_all();
    }

    /// The offset just past everything seen so far: a cursor that skips the past.
    pub fn end_offset(&self) -> u64 {
        let s = self.lock();
        s.start + s.buf.len() as u64
    }

    /// Wait up to `timeout` for output beyond `*cursor`, and move the cursor past it.
    /// A zero timeout only looks.
    pub fn read(&self, cursor: &mut u64, timeout: Duration) -> Read {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            let mut lost = false;
            if *cursor < s.start {
                *cursor = s.start;
                lost = true;
            }
            let have = s.start + s.buf.len() as u64;
            if *cursor < have {
                let skip = (*cursor - s.start) as usize;
                let bytes: Vec<u8> = s.buf.iter().skip(skip).copied().collect();
                *cursor = have;
                return Read::Data { bytes, lost };
            }
            if let Some(reason) = &s.ended {
                return Read::Ended(reason.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Read::Timeout;
            }
            s = self
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Wait for the session to finish connecting.
    pub fn wait_connected(&self, timeout: Duration) -> Result<(), Wait> {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            if let Some(reason) = &s.ended {
                return Err(Wait::Ended(reason.clone()));
            }
            if s.connected {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Wait::Timeout);
            }
            s = self
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const NOW: Duration = Duration::ZERO;

    #[test]
    fn a_cursor_sees_only_what_came_after_it() {
        let t = Tap::default();
        t.push(b"old output\n");
        let mut c = t.end_offset();
        assert_eq!(t.read(&mut c, NOW), Read::Timeout);
        t.push(b"new");
        t.push(b" stuff");
        assert_eq!(
            t.read(&mut c, NOW),
            Read::Data {
                bytes: b"new stuff".to_vec(),
                lost: false
            }
        );
        assert_eq!(t.read(&mut c, NOW), Read::Timeout);
        // A cursor at zero gets the whole history.
        let mut from_start = 0;
        assert!(matches!(t.read(&mut from_start, NOW), Read::Data { bytes, .. } if bytes == b"old output\nnew stuff"));
    }

    #[test]
    fn a_reader_that_falls_behind_is_told_and_skips_ahead() {
        let t = Tap::default();
        t.push(&vec![b'a'; CAPACITY]);
        t.push(b"tail");
        let mut c = 0;
        match t.read(&mut c, NOW) {
            Read::Data { bytes, lost } => {
                assert!(lost);
                assert_eq!(bytes.len(), CAPACITY);
                assert!(bytes.ends_with(b"tail"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn waiting_wakes_when_output_arrives() {
        let t = Arc::new(Tap::default());
        let mut c = 0;
        let writer = t.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.push(b"hello");
        });
        let started = Instant::now();
        let got = t.read(&mut c, Duration::from_secs(5));
        assert!(matches!(got, Read::Data { ref bytes, .. } if bytes == b"hello"));
        assert!(started.elapsed() < Duration::from_secs(2));
        h.join().unwrap();
    }

    #[test]
    fn an_ended_session_hands_over_its_last_output_first() {
        let t = Tap::default();
        t.push(b"bye");
        t.end(Some("connection reset".into()));
        let mut c = 0;
        assert!(matches!(t.read(&mut c, NOW), Read::Data { .. }));
        assert_eq!(t.read(&mut c, NOW), Read::Ended(Some("connection reset".into())));
        assert!(t.is_ended() && !t.is_connected());
        t.end(None); // the first reason stands
        assert_eq!(t.read(&mut c, NOW), Read::Ended(Some("connection reset".into())));
    }

    #[test]
    fn waiting_for_a_connection() {
        let t = Arc::new(Tap::default());
        assert_eq!(t.wait_connected(Duration::from_millis(20)), Err(Wait::Timeout));
        let other = t.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.set_connected();
        });
        assert_eq!(t.wait_connected(Duration::from_secs(5)), Ok(()));

        let dead = Tap::default();
        dead.end(Some("refused".into()));
        assert_eq!(
            dead.wait_connected(Duration::from_secs(1)),
            Err(Wait::Ended(Some("refused".into())))
        );
    }
}
