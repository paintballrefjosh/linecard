//! Finding a Python 3 to run scripts with.
//!
//! Linecard doesn't ship Python; it uses the one on the computer (or the one
//! named in Settings). Scripts run in their own process, so a mistake in one
//! can't take the app down, and Stop really stops it.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The oldest Python the bundled module supports.
pub const MIN_VERSION: (u32, u32) = (3, 8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Python {
    pub program: PathBuf,
    /// Arguments that go before the script (`-3` for the Windows `py` launcher).
    pub args: Vec<String>,
    /// e.g. "3.11.9"
    pub version: String,
}

impl Python {
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        hide_window(&mut c);
        c
    }
}

/// Don't flash a console window for each script on Windows.
pub(crate) fn hide_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// `custom` is the path chosen in Settings (empty or `None`: look for one).
pub fn find(custom: Option<&str>) -> Result<Python, String> {
    if let Some(path) = custom.map(str::trim).filter(|p| !p.is_empty()) {
        return probe(path, &[]).map_err(|e| format!("{path}: {e}"));
    }
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python", &[]), ("python3", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    let mut why = Vec::new();
    for (program, args) in candidates {
        match probe(program, args) {
            Ok(p) => return Ok(p),
            Err(e) => why.push(format!("{program}: {e}")),
        }
    }
    Err(format!(
        "Python {}.{} or newer was not found ({}). Install it from python.org, or set its path in Settings.",
        MIN_VERSION.0,
        MIN_VERSION.1,
        why.join("; ")
    ))
}

fn probe(program: &str, args: &[&str]) -> Result<Python, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .args(["-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    // Windows ships a `python.exe` stub that can hang or open the Store; don't wait on it.
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed() > Duration::from_secs(5) => {
                let _ = child.kill();
                return Err("did not answer".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    if !status.success() {
        return Err("not a working Python".into());
    }
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    let version = out.trim().to_string();
    let parsed = parse_version(&version).ok_or_else(|| format!("unexpected answer {version:?}"))?;
    if parsed < MIN_VERSION {
        return Err(format!(
            "Python {version} is too old (need {}.{} or newer)",
            MIN_VERSION.0, MIN_VERSION.1
        ));
    }
    Ok(Python {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        version,
    })
}

fn parse_version(v: &str) -> Option<(u32, u32)> {
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("3.11.9"), Some((3, 11)));
        assert_eq!(parse_version("2.7.18"), Some((2, 7)));
        assert_eq!(parse_version("nope"), None);
        assert!((3, 7) < MIN_VERSION && (3, 8) >= MIN_VERSION && (3, 12) >= MIN_VERSION);
    }

    #[test]
    fn a_bad_custom_path_says_which_one() {
        let e = find(Some("/definitely/not/python")).unwrap_err();
        assert!(e.starts_with("/definitely/not/python:"), "{e}");
    }
}
