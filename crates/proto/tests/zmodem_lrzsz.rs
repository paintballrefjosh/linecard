//! ZMODEM against the reference implementation: lrzsz's `sz` and `rz`, run
//! as child processes with their stdin and stdout as the line. Skipped when
//! lrzsz isn't installed (CI installs it on Linux).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use linecard_proto::xmodem::Link;
use linecard_proto::zmodem::{self, Options};

fn have(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// A child process's stdio as a line.
struct ChildLink {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Vec<u8>>,
    held: VecDeque<u8>,
}

impl ChildLink {
    fn spawn(cmd: &mut Command) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start lrzsz");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        ChildLink {
            child,
            stdin,
            rx,
            held: VecDeque::new(),
        }
    }
}

impl Link for ChildLink {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.stdin.write_all(data)?;
        self.stdin.flush()
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

fn sample(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 131 + i / 5) as u8).collect()
}

#[test]
fn receives_from_sz() {
    if !have("sz") {
        eprintln!("lrzsz not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let data = sample(300 * 1024 + 17);
    let src = dir.path().join("from-server.bin");
    std::fs::write(&src, &data).unwrap();
    let out = dir.path().join("saved");
    std::fs::create_dir(&out).unwrap();

    let mut link = ChildLink::spawn(Command::new("sz").arg("-b").arg(&src));
    let cancel = AtomicBool::new(false);
    let files = zmodem::receive(&mut link, &out, Options::default(), &cancel, &mut |_, _, _| {}).unwrap();
    let status = link.child.wait().unwrap();
    assert!(status.success(), "sz exited with {status}");
    assert_eq!(files, [out.join("from-server.bin")]);
    assert!(std::fs::read(&files[0]).unwrap() == data);
}

#[test]
fn sends_to_rz() {
    if !have("rz") {
        eprintln!("lrzsz not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let data = sample(500 * 1024 + 3);
    let src = dir.path().join("upload.bin");
    std::fs::write(&src, &data).unwrap();
    let into = dir.path().join("server");
    std::fs::create_dir(&into).unwrap();

    for escape_ctl in [false, true] {
        let mut link = ChildLink::spawn(Command::new("rz").args(["-b", "-y"]).current_dir(&into));
        let cancel = AtomicBool::new(false);
        let mut f = File::open(&src).unwrap();
        let sent = zmodem::send(
            &mut link,
            &mut f,
            "upload.bin",
            Options { escape_ctl },
            &cancel,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(sent, data.len() as u64);
        let status = link.child.wait().unwrap();
        assert!(status.success(), "rz exited with {status}");
        assert!(
            std::fs::read(into.join("upload.bin")).unwrap() == data,
            "escape_ctl {escape_ctl}"
        );
    }
}
