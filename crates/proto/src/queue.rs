//! Writes that can block (a serial line at 9600 baud, a Telnet server that
//! stopped reading) run on a thread of their own, in order, so sending input
//! never holds up the caller. The app calls `Session::write` from its main
//! thread, so a write that blocked there would freeze the whole window.

use std::io;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

type Job<T> = Box<dyn FnOnce(&mut T) -> io::Result<()> + Send>;

pub(crate) struct WriteQueue<T> {
    jobs: Option<Sender<Job<T>>>,
    /// The latest failure, reported once by the next call.
    failed: Arc<Mutex<Option<(io::ErrorKind, String)>>>,
}

/// Another handle on the same queue and thread.
impl<T> Clone for WriteQueue<T> {
    fn clone(&self) -> Self {
        WriteQueue {
            jobs: self.jobs.clone(),
            failed: self.failed.clone(),
        }
    }
}

impl<T: Send + 'static> WriteQueue<T> {
    pub(crate) fn spawn(name: &str, mut target: T) -> io::Result<Self> {
        let (tx, rx) = channel::<Job<T>>();
        let failed: Arc<Mutex<Option<(io::ErrorKind, String)>>> = Arc::default();
        let fail = failed.clone();
        thread::Builder::new().name(name.into()).spawn(move || {
            for job in rx {
                if let Err(e) = job(&mut target) {
                    if let Ok(mut f) = fail.lock() {
                        *f = Some((e.kind(), e.to_string()));
                    }
                }
            }
        })?;
        Ok(WriteQueue { jobs: Some(tx), failed })
    }

    /// Queue `job`. If an earlier one failed, that error is returned instead
    /// (once; the queue carries on, as a failed write did before it had one).
    pub(crate) fn run(&self, job: impl FnOnce(&mut T) -> io::Result<()> + Send + 'static) -> io::Result<()> {
        if let Some((kind, msg)) = self.failed.lock().ok().and_then(|mut f| f.take()) {
            return Err(io::Error::new(kind, msg));
        }
        self.jobs
            .as_ref()
            .and_then(|j| j.send(Box::new(job)).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "session closed"))
    }

    /// Stop taking jobs through this handle; queued ones still run, and the
    /// thread ends once every handle is closed or dropped.
    pub(crate) fn close(&mut self) {
        self.jobs = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn jobs_run_in_order_without_blocking_the_caller() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (out_tx, out_rx) = mpsc::channel::<u8>();
        let q = WriteQueue::spawn("test", (gate_rx, out_tx)).unwrap();
        let start = Instant::now();
        // The first write is stuck until the gate opens; the rest still queue at once.
        q.run(|(gate, out)| {
            gate.recv().ok();
            out.send(1).ok();
            Ok(())
        })
        .unwrap();
        for n in 2..=4u8 {
            q.run(move |(_, out)| {
                out.send(n).ok();
                Ok(())
            })
            .unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(1), "queuing didn't wait");
        gate_tx.send(()).unwrap();
        let got: Vec<u8> = (0..4)
            .map(|_| out_rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, [1, 2, 3, 4]);
    }

    #[test]
    fn a_failure_is_reported_by_the_next_call() {
        let q = WriteQueue::spawn("test", ()).unwrap();
        q.run(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "line stuck")))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let e = loop {
            match q.run(|_| Ok(())) {
                Err(e) => break e,
                Ok(()) => {
                    assert!(Instant::now() < deadline, "the failure never surfaced");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(q.run(|_| Ok(())).is_ok(), "reported once, and later writes still go");
    }

    #[test]
    fn closed_queues_refuse_new_jobs() {
        let mut q = WriteQueue::spawn("test", ()).unwrap();
        q.close();
        assert_eq!(q.run(|_| Ok(())).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
}
