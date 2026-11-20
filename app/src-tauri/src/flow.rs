//! Flow control for session output sent to the window.
//!
//! Tauri keeps every message of 1 KB or more in this process until the
//! window fetches it, and queues smaller ones as scripts for the window to
//! run. Neither has a limit: if the window stops reading (a frozen or
//! suspended web view), a busy session — a remote desktop's screen updates,
//! or a terminal that never stops printing — grows memory until the computer
//! runs out. One left overnight reached about 100 GB.
//!
//! So the window acknowledges the bytes it receives (`output_ack`), and a
//! session stops sending once `limit` bytes are unacknowledged. What it
//! couldn't send is caught up when the window does read again: a terminal
//! says how much output was skipped, and a remote screen repaints the area
//! that changed. Session logs and scripts still see everything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tauri::ipc::{Channel, InvokeResponseBody};

/// Unread output a terminal may have waiting for the window.
pub const TERMINAL_LIMIT: u64 = 16 << 20;
/// Unread output a remote screen may have waiting (a full 1080p frame is 8 MB).
pub const SCREEN_LIMIT: u64 = 64 << 20;

/// Called when the window has caught up after output was held back.
type Resume = Box<dyn FnMut(&Flow) + Send>;

pub struct Flow {
    channel: Channel<InvokeResponseBody>,
    limit: u64,
    state: Mutex<State>,
    resume: Mutex<Option<Resume>>,
}

#[derive(Default)]
struct State {
    /// Sent but not yet acknowledged by the window.
    unacked: u64,
    /// Output was held back; nothing more goes until the window catches up.
    behind: bool,
}

/// Flows by channel id, for `output_ack`. Weak, so a flow goes when its
/// session does.
fn registry() -> &'static Mutex<HashMap<u32, Weak<Flow>>> {
    static FLOWS: OnceLock<Mutex<HashMap<u32, Weak<Flow>>>> = OnceLock::new();
    FLOWS.get_or_init(Default::default)
}

impl Flow {
    pub fn new(channel: Channel<InvokeResponseBody>, limit: u64) -> Arc<Self> {
        let flow = Arc::new(Flow {
            channel,
            limit,
            state: Mutex::default(),
            resume: Mutex::new(None),
        });
        if let Ok(mut all) = registry().lock() {
            all.retain(|_, f| f.strong_count() > 0);
            all.insert(flow.channel.id(), Arc::downgrade(&flow));
        }
        flow
    }

    /// What to do once the window catches up (send what was held back).
    pub fn on_resume(&self, f: impl FnMut(&Flow) + Send + 'static) {
        if let Ok(mut r) = self.resume.lock() {
            *r = Some(Box::new(f));
        }
    }

    /// Send bytes to the window, unless too many are waiting unread; then
    /// returns false and the caller keeps track of what was held back. A
    /// message bigger than the limit still goes when nothing is waiting.
    pub fn send(&self, bytes: Vec<u8>) -> bool {
        let len = bytes.len() as u64;
        {
            let Ok(mut s) = self.state.lock() else { return false };
            if s.behind || (s.unacked > 0 && s.unacked + len > self.limit) {
                s.behind = true;
                return false;
            }
            s.unacked += len;
        }
        let _ = self.channel.send(InvokeResponseBody::Raw(bytes));
        true
    }

    /// Send even when behind (what catches the window up).
    fn send_now(&self, bytes: Vec<u8>) {
        if let Ok(mut s) = self.state.lock() {
            s.unacked += bytes.len() as u64;
        }
        let _ = self.channel.send(InvokeResponseBody::Raw(bytes));
    }

    /// Send what was held back, from the resume handler. Holds it back again
    /// (returning false) if the window still has too much to read.
    pub fn catch_up(&self, bytes: Vec<u8>) -> bool {
        let room = self
            .state
            .lock()
            .map(|s| s.unacked == 0 || s.unacked + bytes.len() as u64 <= self.limit)
            .unwrap_or(false);
        if room {
            self.send_now(bytes);
        } else if let Ok(mut s) = self.state.lock() {
            s.behind = true;
        }
        room
    }

    /// The window read `bytes`. Once it's down to a quarter of the limit,
    /// output that was held back is caught up.
    pub fn ack(&self, bytes: u64) {
        let resume = {
            let Ok(mut s) = self.state.lock() else { return };
            s.unacked = s.unacked.saturating_sub(bytes);
            let go = s.behind && s.unacked <= self.limit / 4;
            if go {
                s.behind = false;
            }
            go
        };
        if resume {
            if let Ok(mut r) = self.resume.lock() {
                if let Some(f) = r.as_mut() {
                    f(self);
                }
            }
        }
    }

    #[cfg(test)]
    fn unacked(&self) -> u64 {
        self.state.lock().unwrap().unacked
    }
}

/// The window received `bytes` of output on channel `channel`.
#[tauri::command]
pub fn output_ack(channel: u32, bytes: u64) {
    let flow = registry().lock().ok().and_then(|all| all.get(&channel)?.upgrade());
    if let Some(f) = flow {
        f.ack(bytes);
    }
}

/// Bookkeeping for a terminal: bytes skipped since the window fell behind.
#[derive(Default)]
pub struct Skipped(Mutex<u64>);

impl Skipped {
    pub fn add(&self, n: usize) {
        if let Ok(mut s) = self.0.lock() {
            *s += n as u64;
        }
    }

    pub fn take(&self) -> u64 {
        self.0.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or(0)
    }
}

/// The line a terminal shows where output was skipped.
pub fn skipped_note(bytes: u64) -> Vec<u8> {
    let size = if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    };
    format!("\x1b[0m\r\n\x1b[2m[{size} of output skipped: the window wasn't keeping up]\x1b[0m\r\n").into_bytes()
}

/// A terminal's flow: output is skipped while the window isn't reading, and
/// a note of how much is shown when it catches up.
pub fn terminal(channel: Channel<InvokeResponseBody>) -> (Arc<Flow>, Arc<Skipped>) {
    let flow = Flow::new(channel, TERMINAL_LIMIT);
    let skipped = Arc::new(Skipped::default());
    let s = skipped.clone();
    flow.on_resume(move |f| {
        let n = s.take();
        if n > 0 && !f.catch_up(skipped_note(n)) {
            s.add(n as usize);
        }
    });
    (flow, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn channel(sent: Arc<AtomicUsize>) -> Channel<InvokeResponseBody> {
        Channel::new(move |body| {
            if let InvokeResponseBody::Raw(b) = body {
                sent.fetch_add(b.len(), Ordering::SeqCst);
            }
            Ok(())
        })
    }

    #[test]
    fn holds_back_past_the_limit_and_resumes_on_ack() {
        let sent = Arc::new(AtomicUsize::new(0));
        let flow = Flow::new(channel(sent.clone()), 100);
        let resumed = Arc::new(AtomicUsize::new(0));
        let r = resumed.clone();
        flow.on_resume(move |_| {
            r.fetch_add(1, Ordering::SeqCst);
        });

        assert!(flow.send(vec![0; 60]));
        assert!(flow.send(vec![0; 40]));
        // The window hasn't read anything: the next send is held back, and
        // so is everything after it, even small, until the window catches up.
        assert!(!flow.send(vec![0; 1]));
        assert_eq!(sent.load(Ordering::SeqCst), 100);
        flow.ack(50);
        assert!(!flow.send(vec![0; 1]), "still behind at 50 of 100");
        assert_eq!(resumed.load(Ordering::SeqCst), 0);
        flow.ack(30);
        assert_eq!(
            resumed.load(Ordering::SeqCst),
            1,
            "a quarter of the limit left: caught up"
        );
        assert!(flow.send(vec![0; 10]));
        assert_eq!(flow.unacked(), 30);
    }

    #[test]
    fn a_message_bigger_than_the_limit_goes_when_nothing_waits() {
        let sent = Arc::new(AtomicUsize::new(0));
        let flow = Flow::new(channel(sent.clone()), 100);
        assert!(flow.send(vec![0; 500]));
        assert!(!flow.send(vec![0; 1]));
        flow.ack(500);
        assert!(flow.send(vec![0; 1]));
    }

    #[test]
    fn memory_stays_bounded_when_the_window_never_reads() {
        let sent = Arc::new(AtomicUsize::new(0));
        let (flow, skipped) = terminal(channel(sent.clone()));
        let chunk = 64 * 1024;
        for _ in 0..10_000 {
            if !flow.send(vec![b'x'; chunk]) {
                skipped.add(chunk);
            }
        }
        // 625 MB printed; at most the limit went to the window.
        assert!(sent.load(Ordering::SeqCst) as u64 <= TERMINAL_LIMIT);
        let held = skipped.0.lock().map(|s| *s).unwrap();
        assert_eq!(held + sent.load(Ordering::SeqCst) as u64, 10_000 * chunk as u64);

        // The window reads it all: the note of what was skipped goes out.
        let before = sent.load(Ordering::SeqCst);
        flow.ack(before as u64);
        assert_eq!(sent.load(Ordering::SeqCst) - before, skipped_note(held).len());
        assert_eq!(skipped.take(), 0);
    }

    #[test]
    fn acks_reach_a_flow_by_channel_id_and_not_after_it_is_gone() {
        let sent = Arc::new(AtomicUsize::new(0));
        let flow = Flow::new(channel(sent), 100);
        let id = flow.channel.id();
        flow.send(vec![0; 80]);
        output_ack(id, 30);
        assert_eq!(flow.unacked(), 50);
        drop(flow);
        output_ack(id, 30); // no flow, no panic
    }

    #[test]
    fn note_sizes() {
        assert!(String::from_utf8(skipped_note(1))
            .unwrap()
            .contains("[1 KB of output skipped"));
        assert!(String::from_utf8(skipped_note(3 << 20))
            .unwrap()
            .contains("[3.0 MB of output skipped"));
    }
}
