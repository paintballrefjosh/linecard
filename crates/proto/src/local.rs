//! Local shells (PowerShell, cmd, WSL, Git Bash; bash, zsh, fish…) in a
//! pseudo-terminal: ConPTY on Windows, openpty elsewhere.
//!
//! Shells are found on this computer and opened by id, so the window can only
//! start what's on the list, never an arbitrary program.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use linecard_core_model::ConnectionKind;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::queue::WriteQueue;
use crate::{EventSink, Session, SessionEvent};

/// A shell this computer can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell {
    /// Stable on this computer: "pwsh", "cmd", "wsl:Ubuntu", "/bin/zsh"…
    pub id: String,
    /// "PowerShell 7", "Ubuntu (WSL)", "zsh"…
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl LocalShell {
    fn new(id: impl Into<String>, name: impl Into<String>, program: impl Into<PathBuf>, args: &[&str]) -> Self {
        LocalShell {
            id: id.into(),
            name: name.into(),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
}

/// The shells on this computer, the usual default first.
pub fn local_shells() -> Vec<LocalShell> {
    #[cfg(windows)]
    {
        windows_shells()
    }
    #[cfg(not(windows))]
    {
        unix_shells()
    }
}

/// `name` in a folder on PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(windows)]
fn windows_shells() -> Vec<LocalShell> {
    let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
    let system32 = root.join("System32");
    let mut out = Vec::new();
    let pwsh = on_path("pwsh.exe").or_else(|| {
        let p = PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()))
            .join("PowerShell\\7\\pwsh.exe");
        p.is_file().then_some(p)
    });
    if let Some(p) = pwsh {
        out.push(LocalShell::new("pwsh", "PowerShell 7", p, &["-NoLogo"]));
    }
    let ps = system32.join("WindowsPowerShell\\v1.0\\powershell.exe");
    if ps.is_file() {
        out.push(LocalShell::new("powershell", "Windows PowerShell", ps, &["-NoLogo"]));
    }
    let cmd = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| system32.join("cmd.exe"));
    if cmd.is_file() {
        out.push(LocalShell::new("cmd", "Command Prompt", cmd, &[]));
    }
    let wsl = system32.join("wsl.exe");
    if wsl.is_file() {
        for distro in wsl_distros(&wsl) {
            out.push(LocalShell::new(
                format!("wsl:{distro}"),
                format!("{distro} (WSL)"),
                &wsl,
                &["-d", &distro, "--cd", "~"],
            ));
        }
    }
    for base in [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("LOCALAPPDATA").map(|l| {
            let mut p = PathBuf::from(l);
            p.push("Programs");
            p.into_os_string()
        }),
    ]
    .into_iter()
    .flatten()
    {
        let bash = PathBuf::from(base).join("Git\\bin\\bash.exe");
        if bash.is_file() {
            out.push(LocalShell::new("gitbash", "Git Bash", bash, &["--login", "-i"]));
            break;
        }
    }
    out
}

/// Installed WSL distributions. `wsl -l -q` answers in UTF-16.
#[cfg(windows)]
fn wsl_distros(wsl: &Path) -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new(wsl)
        .args(["-l", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new(); // WSL is there but no distribution is installed
    }
    parse_wsl_list(&out.stdout)
}

/// The lines of `wsl -l -q` output (UTF-16LE, sometimes with a BOM or NULs).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_wsl_list(raw: &[u8]) -> Vec<String> {
    let (pairs, _) = raw.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|&c| u16::from_le_bytes(c)).collect();
    String::from_utf16_lossy(&units)
        .lines()
        .map(|l| {
            l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0')
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(not(windows))]
fn unix_shells() -> Vec<LocalShell> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut add = |p: PathBuf| {
        let Ok(real) = std::fs::canonicalize(&p) else { return };
        if !real.is_file() || !seen.insert(real) {
            return;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = p.to_string_lossy().into_owned();
        out.push(LocalShell::new(id, name, p, &["-l"]));
    };
    // The user's own shell first.
    if let Some(s) = std::env::var_os("SHELL") {
        add(PathBuf::from(s));
    }
    for name in ["zsh", "bash", "fish", "sh"] {
        for dir in ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
            add(Path::new(dir).join(name));
        }
        if let Some(p) = on_path(name) {
            add(p);
        }
    }
    out
}

/// A shell running in a pseudo-terminal on this computer.
pub struct LocalSession {
    /// Dropped when the shell exits (that ends the reader on Windows).
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    out: WriteQueue<Box<dyn Write + Send>>,
}

impl LocalSession {
    /// Start the shell with this id from [`local_shells`].
    pub fn spawn(shell_id: &str, cols: u16, rows: u16, events: EventSink) -> io::Result<Self> {
        let shell = local_shells().into_iter().find(|s| s.id == shell_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no shell \"{shell_id}\" on this computer"),
            )
        })?;
        Self::spawn_shell(&shell, cols, rows, events)
    }

    pub fn spawn_shell(shell: &LocalShell, cols: u16, rows: u16, mut events: EventSink) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        let mut cmd = CommandBuilder::new(&shell.program);
        cmd.args(&shell.args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Linecard");
        if let Some(home) = home_dir() {
            cmd.cwd(home);
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        // The shell holds its own end now; ours would keep the terminal open.
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;
        let out = WriteQueue::spawn("local-tx", writer)?;
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let exit: Arc<Mutex<Option<u32>>> = Arc::default();

        events(SessionEvent::Connected);
        {
            // On Windows the output pipe stays open after the shell exits until
            // the pseudo-console closes, so close it once the shell is gone.
            let (master, exit) = (master.clone(), exit.clone());
            thread::Builder::new().name("local-wait".into()).spawn(move || {
                let code = wait(&mut child);
                *exit.lock().unwrap_or_else(|p| p.into_inner()) = code;
                master.lock().unwrap_or_else(|p| p.into_inner()).take();
            })?;
        }
        thread::Builder::new().name("local-rx".into()).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => events(SessionEvent::Data(buf[..n].to_vec())),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let code = *exit.lock().unwrap_or_else(|p| p.into_inner());
            let reason = match code {
                Some(0) | None => None,
                Some(c) => Some(format!("the shell exited with code {c}")),
            };
            events(SessionEvent::Disconnected { reason });
        })?;
        Ok(LocalSession { master, killer, out })
    }
}

fn wait(child: &mut Box<dyn Child + Send + Sync>) -> Option<u32> {
    child.wait().ok().map(|s| s.exit_code())
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from).filter(|p| p.is_dir())
}

impl Session for LocalSession {
    fn kind(&self) -> ConnectionKind {
        // Local shells aren't saved connections; the nearest terminal kind.
        ConnectionKind::Ssh
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let data = data.to_vec();
        self.out.run(move |w| {
            w.write_all(&data)?;
            w.flush()
        })
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        match &*self.master.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(m) => m
                .resize(PtySize {
                    rows: rows.max(1),
                    cols: cols.max(1),
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(io::Error::other),
            None => Ok(()), // the shell has exited
        }
    }

    fn close(&mut self) -> io::Result<()> {
        self.out.close();
        let _ = self.killer.kill();
        Ok(())
    }
}

impl Drop for LocalSession {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn wsl_lists_are_read_from_utf16() {
        let text = "\u{feff}Ubuntu\r\ndocker-desktop\r\n\r\n";
        let raw: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(parse_wsl_list(&raw), ["Ubuntu", "docker-desktop"]);
    }

    #[test]
    fn this_computer_has_a_shell() {
        let shells = local_shells();
        assert!(!shells.is_empty(), "no shell found");
        let mut ids: Vec<&str> = shells.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), shells.len(), "ids are unique: {shells:?}");
    }

    #[test]
    fn unknown_shells_are_refused() {
        let Err(e) = LocalSession::spawn("not-a-shell", 80, 24, Box::new(|_| {})) else {
            panic!("started something");
        };
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    /// Runs a real shell: types a command, reads its output, exits.
    #[test]
    fn a_shell_runs_commands_and_reports_its_exit() {
        let shell = local_shells()
            .into_iter()
            .find(|s| s.id == "cmd" || s.id.ends_with("sh"))
            .expect("a shell");
        let (tx, rx) = mpsc::channel();
        let mut s = LocalSession::spawn_shell(
            &shell,
            100,
            30,
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .unwrap();
        s.write(b"echo linecard-$((6*7))%LC%\r").unwrap();
        s.resize(120, 40).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        let (mut answered, mut ended) = (false, false);
        while Instant::now() < deadline && !ended {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(SessionEvent::Data(d)) => {
                    // ConPTY asks where the cursor is before it shows anything
                    // (xterm.js answers this in the app).
                    if d.windows(4).any(|w| w == b"\x1b[6n") {
                        s.write(b"\x1b[1;1R").unwrap();
                    }
                    seen.extend(d);
                    let text = String::from_utf8_lossy(&seen);
                    // bash and zsh do the arithmetic; cmd leaves %LC% alone.
                    // (cmd's line is there twice: as typed, then as printed.)
                    if !answered && (text.contains("linecard-42") || text.matches("linecard-$((6*7))%LC%").count() >= 2)
                    {
                        answered = true;
                        s.write(b"exit\r").unwrap();
                    }
                }
                Ok(SessionEvent::Disconnected { .. }) => ended = true,
                _ => {}
            }
        }
        assert!(
            answered,
            "no output from {}: {:?}",
            shell.name,
            String::from_utf8_lossy(&seen)
        );
        assert!(ended, "the session didn't end after exit");
    }
}
