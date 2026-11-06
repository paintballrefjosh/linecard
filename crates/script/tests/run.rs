//! Real Python scripts against a pretend device.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linecard_script::{python, ConnectionInfo, Event, Host, Options, Outcome, Python, Run, SessionInfo, Tap};

/// Python, or None when this machine has none (CI always does).
fn python() -> Option<Python> {
    match python::find(None) {
        Ok(p) => Some(p),
        Err(e) => {
            assert!(std::env::var_os("CI").is_none(), "CI needs Python: {e}");
            eprintln!("skipping: {e}");
            None
        }
    }
}

/// A router: echoes what is typed, answers `show version`, shows a prompt.
struct Fake {
    sessions: Mutex<Vec<(SessionInfo, Arc<Tap>)>>,
    typed: Mutex<Vec<String>>,
    asked: Mutex<Vec<String>>,
}

fn session(id: &str, name: &str, active: bool) -> (SessionInfo, Arc<Tap>) {
    let info = SessionInfo {
        id: id.into(),
        name: name.into(),
        kind: "ssh".into(),
        endpoint: "10.0.0.1:22".into(),
        connection: None,
        active,
        connected: true,
    };
    let tap = Arc::new(Tap::default());
    tap.set_connected();
    (info, tap)
}

impl Fake {
    fn new() -> Arc<Fake> {
        let (info, tap) = session("s1", "edge1", true);
        tap.push(b"edge1 login ok\r\nRouter#");
        Arc::new(Fake {
            sessions: Mutex::new(vec![(info, tap)]),
            typed: Mutex::default(),
            asked: Mutex::default(),
        })
    }
}

impl Host for Fake {
    fn sessions(&self) -> Vec<SessionInfo> {
        self.sessions.lock().unwrap().iter().map(|(i, _)| i.clone()).collect()
    }
    fn connections(&self) -> Vec<ConnectionInfo> {
        vec![ConnectionInfo {
            id: "c-spine".into(),
            name: "spine01".into(),
            path: "Lab/spine01".into(),
            kind: "ssh".into(),
            endpoint: "10.0.0.2:22".into(),
        }]
    }
    fn tap(&self, session: &str) -> Option<Arc<Tap>> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|(i, _)| i.id == session)
            .map(|(_, t)| t.clone())
    }
    fn write(&self, session: &str, data: &[u8]) -> Result<(), String> {
        let tap = self.tap(session).ok_or("closed")?;
        let text = String::from_utf8_lossy(data).to_string();
        self.typed.lock().unwrap().push(text.clone());
        if let Some(line) = text.strip_suffix('\r') {
            tap.push(format!("{line}\r\n").as_bytes()); // the echo
            match line {
                "show version" => tap.push(b"Cisco IOS 15.2\r\nuptime 4 days\r\n"),
                "colors" => tap.push(b"\x1b[1;32mgreen\x1b[0m \x1b]0;title\x07done\r\n"),
                "split" => {
                    // An escape sequence arriving in two pieces.
                    tap.push(b"before \x1b[3");
                    std::thread::sleep(Duration::from_millis(50));
                    tap.push(b"1mred\x1b[0m after\r\n");
                }
                "drop" => {
                    tap.push(b"going down\r\n");
                    tap.end(Some("connection reset".into()));
                    return Ok(());
                }
                _ => {}
            }
            tap.push(b"Router#");
        }
        Ok(())
    }
    fn open(&self, connection: &str, _timeout: Duration) -> Result<String, String> {
        assert_eq!(connection, "c-spine");
        let (mut info, _) = session("s2", "spine01", false);
        info.connection = Some(connection.into());
        info.connected = false;
        // A session that takes a moment to log in, banner first.
        let fresh = Arc::new(Tap::default());
        let f = fresh.clone();
        fresh.push(b"Welcome to spine01\r\n");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            f.push(b"spine01#");
            f.set_connected();
        });
        self.sessions.lock().unwrap().push((info, fresh));
        Ok("s2".into())
    }
    fn screen(&self, session: &str) -> Result<String, String> {
        self.tap(session).ok_or("closed")?;
        Ok(format!(
            "SCREEN of {session}
line two"
        ))
    }
    fn close(&self, session: &str) -> Result<(), String> {
        self.sessions.lock().unwrap().retain(|(i, _)| i.id != session);
        Ok(())
    }
    fn ask(&self, _run: u64, prompt: &str, secret: bool) -> Option<String> {
        self.asked.lock().unwrap().push(format!("{prompt}|{secret}"));
        (prompt != "cancel me").then(|| format!("answer to {prompt}"))
    }
}

struct Ran {
    out: Vec<String>,
    err: Vec<String>,
    outcome: Outcome,
}

fn start(host: Arc<Fake>, source: &str, dir: &tempfile::TempDir) -> (Run, mpsc::Receiver<Event>) {
    let script = dir.path().join("test.py");
    std::fs::write(&script, source).unwrap();
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let run = Run::start(
        linecard_script::run::next_id(),
        host,
        &Options {
            python: python().unwrap(),
            script,
            args: vec![],
            lib_dir: dir.path().join("lib"),
        },
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    )
    .unwrap();
    (run, rx)
}

fn run_script(host: Arc<Fake>, source: &str) -> Ran {
    let dir = tempfile::tempdir().unwrap();
    let (run, rx) = start(host, source, &dir);
    let outcome = run.wait(Duration::from_secs(60)).expect("script finished");
    let (mut out, mut err) = (vec![], vec![]);
    while let Ok(e) = rx.try_recv() {
        if let Event::Output { stderr, text } = e {
            if stderr { &mut err } else { &mut out }.push(text);
        }
    }
    Ran { out, err, outcome }
}

#[test]
fn a_script_prints_and_finishes() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        "print('hello')\nprint('two')\nimport sys\nprint('oops', file=sys.stderr)\n",
    );
    assert_eq!(r.out, ["hello", "two"]);
    assert_eq!(r.err, ["oops"]);
    assert_eq!(
        r.outcome,
        Outcome {
            code: Some(0),
            stopped: false
        }
    );
}

#[test]
fn a_line_that_never_ends_comes_through_in_pieces() {
    if python().is_none() {
        return;
    }
    // 200 000 characters with no newline, then a normal line.
    let r = run_script(
        Fake::new(),
        "import sys\nsys.stdout.write('x' * 200000)\nsys.stdout.flush()\nprint()\nprint('after')\n",
    );
    let pieces: Vec<usize> = r.out.iter().map(String::len).collect();
    assert!(pieces.len() >= 4, "split into 64 KiB pieces: {pieces:?}");
    assert!(pieces.iter().all(|&n| n <= 64 * 1024), "{pieces:?}");
    assert_eq!(
        r.out
            .iter()
            .filter(|l| l.starts_with('x'))
            .map(String::len)
            .sum::<usize>(),
        200_000
    );
    assert_eq!(r.out.last().map(String::as_str), Some("after"));
}

#[test]
fn an_error_in_the_script_is_reported_with_its_exit_code() {
    if python().is_none() {
        return;
    }
    let r = run_script(Fake::new(), "raise SystemExit(3)\n");
    assert_eq!(r.outcome.code, Some(3));
    let r = run_script(Fake::new(), "x = 1 / 0\n");
    assert_eq!(r.outcome.code, Some(1));
    assert!(r.err.iter().any(|l| l.contains("ZeroDivisionError")), "{:?}", r.err);
}

#[test]
fn send_and_expect_on_the_active_session() {
    if python().is_none() {
        return;
    }
    let host = Fake::new();
    let r = run_script(
        host.clone(),
        r#"
import linecard as lc
s = lc.active()
print(s.name, s.kind)
s.send_line("show version")
m = s.expect("Router#", timeout=5)
print(repr(m.before))
print(m.index, m.text)
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(
        r.out,
        [
            "edge1 ssh",
            // The old prompt was there before the script looked; only the answer shows.
            "'show version\\nCisco IOS 15.2\\nuptime 4 days\\n'",
            "0 Router#",
        ]
    );
    assert_eq!(host.typed.lock().unwrap().as_slice(), ["show version\r"]);
}

#[test]
fn expect_picks_the_earliest_of_several_and_regex_works() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        r#"
import linecard as lc
s = lc.active()
s.send_line("show version")
m = s.expect(["nothing like this", "uptime", "IOS"], timeout=5)
print(m.index, m.text)
m = s.expect(r"(\d+) days", regex=True, timeout=5)
print(m.group(1), m.groups)
print(repr(s.read(0.2)))
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    // "IOS" comes before "uptime" in the output, whatever the list order.
    assert_eq!(r.out[0], "2 IOS");
    assert!(r.out[1].starts_with("4 "), "{:?}", r.out);
    assert_eq!(r.out[2], "'\\nRouter#'");
}

#[test]
fn run_returns_the_output_without_the_echo_or_the_prompt() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        r#"
import linecard as lc
s = lc.active()
s.prompt = r"\S+#\s*$"
print(repr(s.run("show version")))
print(repr(s.run("nothing")))
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(r.out, ["'Cisco IOS 15.2\\nuptime 4 days'", "''"]);
}

#[test]
fn escape_sequences_are_stripped_even_when_split_across_reads() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        r#"
import linecard as lc
s = lc.active()
s.send_line("colors")
s.expect("Router#", timeout=5)
print(repr(s.before))
s.send_line("split")
s.expect("Router#", timeout=5)
print(repr(s.before))
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(r.out[0], "'colors\\ngreen done\\n'");
    assert_eq!(r.out[1], "'split\\nbefore red after\\n'");
}

#[test]
fn a_timeout_raises_and_carries_what_arrived() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        r#"
import linecard as lc
s = lc.active()
s.send_line("show version")
try:
    s.expect("never appears", timeout=0.5)
except lc.Timeout as e:
    print("timed out:", e)
    print(repr(e.buffer[:12]))
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert!(
        r.out[0].starts_with("timed out: timed out after 0.5 s waiting for 'never appears'"),
        "{:?}",
        r.out
    );
    assert_eq!(r.out[1], "'show version'");
}

#[test]
fn a_dropped_session_raises_disconnected_after_its_last_words() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        r#"
import linecard as lc
s = lc.active()
s.send_line("drop")
s.expect("going down", timeout=5)
try:
    s.expect("Router#", timeout=5)
except lc.Disconnected as e:
    print("ended:", e.reason)
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(r.out, ["ended: connection reset"]);
}

#[test]
fn opening_a_saved_connection_waits_for_login_and_keeps_the_banner() {
    if python().is_none() {
        return;
    }
    let host = Fake::new();
    let r = run_script(
        host.clone(),
        r#"
import linecard as lc
print([c["path"] for c in lc.connections()])
s = lc.open("lab/SPINE01", timeout=10)
print(s.name, s.connection)
s.expect("Welcome", timeout=2)
s.expect("spine01#", timeout=2)
print(len(lc.sessions()))
s.close()
print(len(lc.sessions()))
try:
    lc.open("nope")
except lc.LinecardError as e:
    print(e)
"#,
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(
        r.out,
        [
            "['Lab/spine01']",
            "spine01 c-spine",
            "2",
            "1",
            "no saved connection called \"nope\""
        ]
    );
}

#[test]
fn asking_the_user_returns_the_answer_or_none() {
    if python().is_none() {
        return;
    }
    let host = Fake::new();
    let r = run_script(
        host.clone(),
        r#"
import linecard as lc
print(lc.ask("Password for edge1", secret=True))
print(lc.ask("cancel me"))
"#,
    );
    assert_eq!(r.out, ["answer to Password for edge1", "None"]);
    assert_eq!(
        host.asked.lock().unwrap().as_slice(),
        ["Password for edge1|true", "cancel me|false"]
    );
}

#[test]
fn stop_ends_a_script_that_is_waiting() {
    if python().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (run, rx) = start(
        Fake::new(),
        "import linecard as lc\nprint('waiting', flush=True)\nlc.active().expect('never', timeout=600)\n",
        &dir,
    );
    // Wait until it is inside expect().
    loop {
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Event::Output { text, .. } if text == "waiting" => break,
            _ => {}
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    run.stop();
    let outcome = run.wait(Duration::from_secs(10)).expect("stopped");
    assert!(outcome.stopped);
}

#[test]
fn a_script_not_started_by_linecard_says_so() {
    if python().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    linecard_script::install_module(dir.path()).unwrap();
    let out = python()
        .unwrap()
        .command()
        .args([
            "-c",
            "import linecard as lc\ntry:\n    lc.active()\nexcept lc.LinecardError as e:\n    print(e)",
        ])
        .env("PYTHONPATH", dir.path())
        .env_remove("LINECARD_SCRIPT_PORT")
        .env_remove("LINECARD_SCRIPT_TOKEN")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("wasn't started by Linecard"),
        "{text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_connection_without_the_token_is_turned_away() {
    if python().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // The script learns its port and then makes up a token.
    let r = run_script(
        Fake::new(),
        r#"
import os, socket, json
sock = socket.create_connection(("127.0.0.1", int(os.environ["LINECARD_SCRIPT_PORT"])))
f = sock.makefile("rw")
f.write(json.dumps({"id": 0, "op": "hello", "token": "wrong"}) + "\n"); f.flush()
# Turned away: the server hangs up. Windows may report that as a reset
# rather than a clean end of stream; both mean no reply came.
try:
    line = f.readline()
except ConnectionError:
    line = ''
print(repr(line))
"#,
    );
    drop(dir);
    assert_eq!(r.out, ["''"], "stderr: {:?}", r.err);
}

#[test]
fn the_screen_can_be_read() {
    if python().is_none() {
        return;
    }
    let r = run_script(
        Fake::new(),
        "import linecard as lc\ns = lc.active()\nprint(repr(s.screen()))\n",
    );
    assert_eq!(r.err, Vec::<String>::new());
    assert_eq!(r.out, [r"'SCREEN of s1\nline two'"]);
}
