//! Running one script: a Python process plus a private socket it uses to
//! reach the app's sessions.
//!
//! The script imports `linecard` (bundled, see `linecard.py`), which connects
//! back to a loopback port with a one-time token and speaks JSON lines. Each
//! request is answered by the [`Host`]. Output the script prints (stdout and
//! stderr) is passed on line by line.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::host::{resolve, Host};
use crate::python::Python;
use crate::tap::{Read, Wait};

/// The module scripts import.
pub const MODULE: &str = include_str!("linecard.py");
const PROTOCOL: u32 = 1;
/// A wait is cut into pieces this long so Stop is noticed promptly.
const SLICE: Duration = Duration::from_millis(200);
const DEFAULT_OPEN_TIMEOUT: f64 = 60.0;

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// A fresh run number. Taken before [`Run::start`] so the app can set up the
/// run's window and prompts before the script can ask for anything.
pub fn next_id() -> u64 {
    NEXT_RUN.fetch_add(1, Ordering::Relaxed)
}

pub struct Options {
    pub python: Python,
    pub script: PathBuf,
    pub args: Vec<String>,
    /// Where `linecard.py` is written for the script to import.
    pub lib_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Output { stderr: bool, text: String },
    Finished(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The exit code (none if the process was killed).
    pub code: Option<i32>,
    /// Stopped by the user rather than finishing by itself.
    pub stopped: bool,
}

/// A script that is running. Dropping it does not stop the script.
pub struct Run {
    pub id: u64,
    child: Arc<Mutex<Child>>,
    stopped: Arc<AtomicBool>,
    done: Arc<(Mutex<Option<Outcome>>, std::sync::Condvar)>,
}

/// Put the bundled module where scripts can import it.
pub fn install_module(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join("linecard.py");
    if std::fs::read_to_string(&target).map_or(true, |t| t != MODULE) {
        std::fs::write(target, MODULE)?;
    }
    Ok(())
}

impl Run {
    pub fn start(
        id: u64,
        host: Arc<dyn Host>,
        opts: &Options,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> io::Result<Run> {
        install_module(&opts.lib_dir)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let mut raw = [0u8; 24];
        getrandom::getrandom(&mut raw).map_err(|e| io::Error::other(e.to_string()))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

        let mut path = vec![opts.lib_dir.clone()];
        if let Some(existing) = std::env::var_os("PYTHONPATH") {
            path.extend(std::env::split_paths(&existing));
        }
        let mut cmd = opts.python.command();
        cmd.args(["-u", "-X", "utf8"])
            .arg(&opts.script)
            .args(&opts.args)
            .env("PYTHONPATH", std::env::join_paths(path).map_err(io::Error::other)?)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("LINECARD_SCRIPT_PORT", port.to_string())
            .env("LINECARD_SCRIPT_TOKEN", &token)
            .env("LINECARD_SCRIPT_RUN", id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = opts.script.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        let stopped = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let done = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
        let on_event = Arc::new(on_event);

        let readers: Vec<_> = [(stdout.map(Pipe::Out), false), (stderr.map(Pipe::Err), true)]
            .into_iter()
            .filter_map(|(pipe, is_err)| {
                let pipe = pipe?;
                let on_event = on_event.clone();
                Some(std::thread::spawn(move || {
                    let mut r = BufReader::new(pipe);
                    let mut line = Vec::new();
                    while matches!(r.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                        let text = String::from_utf8_lossy(&line)
                            .trim_end_matches(['\r', '\n'])
                            .to_string();
                        line.clear();
                        on_event(Event::Output { stderr: is_err, text });
                    }
                }))
            })
            .collect();

        {
            let (host, exited, stopped) = (host, exited.clone(), stopped.clone());
            std::thread::spawn(move || serve(host, id, &token, listener, &exited, &stopped));
        }

        {
            let (child, stopped, exited, done) = (child.clone(), stopped.clone(), exited, done.clone());
            std::thread::spawn(move || {
                let code = loop {
                    let status = child.lock().unwrap_or_else(|p| p.into_inner()).try_wait();
                    match status {
                        Ok(Some(s)) => break s.code(),
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => break None,
                    }
                };
                exited.store(true, Ordering::SeqCst);
                for r in readers {
                    let _ = r.join();
                }
                let outcome = Outcome {
                    code,
                    stopped: stopped.load(Ordering::SeqCst),
                };
                on_event(Event::Finished(outcome.clone()));
                *done.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
                done.1.notify_all();
            });
        }
        Ok(Run {
            id,
            child,
            stopped,
            done,
        })
    }

    /// End the script now.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.child.lock().unwrap_or_else(|p| p.into_inner()).kill();
    }

    /// Wait for the script to end (all its output has been delivered by then).
    pub fn wait(&self, timeout: Duration) -> Option<Outcome> {
        let deadline = Instant::now() + timeout;
        let mut g = self.done.0.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(o) = &*g {
                return Some(o.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            g = self
                .done
                .1
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Pipe::Out(p) => p.read(buf),
            Pipe::Err(p) => p.read(buf),
        }
    }
}

/// Accept the script's connection, then answer it until it goes away.
fn serve(host: Arc<dyn Host>, run: u64, token: &str, listener: TcpListener, exited: &AtomicBool, stopped: &AtomicBool) {
    let stream = loop {
        match listener.accept() {
            Ok((s, _)) => {
                if let Some(ok) = handshake(s, token) {
                    break ok;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if exited.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return,
        }
    };
    let (mut reader, mut writer) = stream;
    let mut ctx = Ctx {
        host,
        run,
        stopped,
        cursors: HashMap::new(),
    };
    let mut line = String::new();
    loop {
        line.clear();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let reply = match ctx.handle(&req) {
            Ok(v) => json!({ "id": id, "ok": v }),
            Err((kind, message)) => json!({ "id": id, "error": message, "kind": kind }),
        };
        if writeln!(writer, "{reply}").and_then(|()| writer.flush()).is_err() {
            return;
        }
    }
}

/// The first line must carry the token; anything else is somebody else.
fn handshake(stream: TcpStream, token: &str) -> Option<(BufReader<TcpStream>, TcpStream)> {
    stream.set_nonblocking(false).ok()?;
    stream.set_nodelay(true).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let hello: Value = serde_json::from_str(&line).ok()?;
    if hello.get("op")? != "hello" || hello.get("token")? != token {
        return None;
    }
    stream.set_read_timeout(None).ok()?;
    let mut writer = stream;
    let reply = json!({ "id": hello.get("id").cloned().unwrap_or(Value::Null), "ok": { "protocol": PROTOCOL } });
    writeln!(writer, "{reply}").ok()?;
    Some((reader, writer))
}

type Fail = (&'static str, String);

struct Ctx<'a> {
    host: Arc<dyn Host>,
    run: u64,
    stopped: &'a AtomicBool,
    /// Where this script has read up to, per session.
    cursors: HashMap<String, u64>,
}

fn text<'v>(req: &'v Value, key: &str) -> Result<&'v str, Fail> {
    req.get(key)
        .and_then(Value::as_str)
        .ok_or(("other", format!("missing {key:?}")))
}

fn seconds(req: &Value, key: &str, default: f64) -> Duration {
    let s = req.get(key).and_then(Value::as_f64).unwrap_or(default);
    Duration::from_secs_f64(s.clamp(0.0, 24.0 * 3600.0))
}

impl Ctx<'_> {
    fn handle(&mut self, req: &Value) -> Result<Value, Fail> {
        match text(req, "op")? {
            "sessions" => Ok(json!(self.host.sessions())),
            "connections" => Ok(json!(self.host.connections())),
            "active" => self
                .host
                .sessions()
                .into_iter()
                .find(|s| s.active)
                .map(|s| json!(s))
                .ok_or(("other", "no terminal tab is active".into())),
            "open" => self.open(text(req, "connection")?, seconds(req, "timeout", DEFAULT_OPEN_TIMEOUT)),
            "write" => {
                let session = text(req, "session")?;
                let data = B64
                    .decode(text(req, "data")?)
                    .map_err(|e| ("other", format!("bad data: {e}")))?;
                // Start listening before typing, so the answer can't be missed.
                self.cursor(session)?;
                self.host.write(session, &data).map_err(|e| ("closed", e))?;
                Ok(json!({}))
            }
            "read" => self.read(text(req, "session")?, seconds(req, "timeout", 0.0)),
            "flush" => {
                let session = text(req, "session")?;
                let tap = self.tap(session)?;
                self.cursors.insert(session.to_string(), tap.end_offset());
                Ok(json!({}))
            }
            "close" => {
                self.host.close(text(req, "session")?).map_err(|e| ("closed", e))?;
                Ok(json!({}))
            }
            "ask" => {
                let value = self.host.ask(
                    self.run,
                    text(req, "prompt")?,
                    req.get("secret").and_then(Value::as_bool) == Some(true),
                );
                Ok(json!({ "value": value }))
            }
            other => Err(("other", format!("unknown request {other:?}"))),
        }
    }

    fn tap(&self, session: &str) -> Result<Arc<crate::tap::Tap>, Fail> {
        self.host
            .tap(session)
            .ok_or(("closed", "that session is closed".into()))
    }

    /// The script's position in a session; the first use starts from now.
    fn cursor(&mut self, session: &str) -> Result<u64, Fail> {
        if let Some(c) = self.cursors.get(session) {
            return Ok(*c);
        }
        let at = self.tap(session)?.end_offset();
        self.cursors.insert(session.to_string(), at);
        Ok(at)
    }

    fn open(&mut self, query: &str, timeout: Duration) -> Result<Value, Fail> {
        let started = Instant::now();
        let all = self.host.connections();
        let conn = resolve(&all, query).map_err(|e| ("other", e))?;
        let session = self.host.open(&conn.id, timeout).map_err(|e| ("timeout", e))?;
        let tap = self.tap(&session)?;
        // Everything the session printed from the start, banner included.
        self.cursors.insert(session.clone(), 0);
        match tap.wait_connected(timeout.saturating_sub(started.elapsed())) {
            Ok(()) => {}
            Err(Wait::Timeout) => return Err(("timeout", format!("{} did not connect in time", conn.name))),
            Err(Wait::Ended(why)) => {
                return Err((
                    "closed",
                    format!(
                        "{} did not connect{}",
                        conn.name,
                        why.map(|w| format!(": {w}")).unwrap_or_default()
                    ),
                ))
            }
        }
        self.host
            .sessions()
            .into_iter()
            .find(|s| s.id == session)
            .map(|s| json!(s))
            .ok_or(("closed", "the session closed".into()))
    }

    fn read(&mut self, session: &str, timeout: Duration) -> Result<Value, Fail> {
        let tap = self.tap(session)?;
        let mut cursor = self.cursor(session)?;
        let deadline = Instant::now() + timeout;
        let reply = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tap.read(&mut cursor, left.min(SLICE)) {
                Read::Data { bytes, lost } => {
                    break json!({ "data": B64.encode(bytes), "lost": lost, "ended": false });
                }
                Read::Ended(reason) => break json!({ "data": "", "lost": false, "ended": true, "reason": reason }),
                Read::Timeout if left.is_zero() || self.stopped.load(Ordering::SeqCst) => {
                    break json!({ "data": "", "lost": false, "ended": false });
                }
                Read::Timeout => {}
            }
        };
        self.cursors.insert(session.to_string(), cursor);
        Ok(reply)
    }
}
