//! What a script can reach. The app implements [`Host`]; tests use a fake.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::tap::Tap;

/// An open terminal session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    /// "ssh", "telnet" or "serial"
    pub kind: String,
    pub endpoint: String,
    /// The saved connection it came from (none for quick connect).
    pub connection: Option<String>,
    /// The tab the user is looking at.
    pub active: bool,
    pub connected: bool,
}

/// A saved connection a script may open.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub id: String,
    pub name: String,
    /// Folders and name, e.g. "Lab/Core/spine01".
    pub path: String,
    pub kind: String,
    pub endpoint: String,
}

pub trait Host: Send + Sync + 'static {
    fn sessions(&self) -> Vec<SessionInfo>;
    fn connections(&self) -> Vec<ConnectionInfo>;
    /// The output of an open session (none once it is closed).
    fn tap(&self, session: &str) -> Option<Arc<Tap>>;
    /// Type into a session, as the user would.
    fn write(&self, session: &str, data: &[u8]) -> Result<(), String>;
    /// Open a saved connection in a new tab and return its session id as soon
    /// as the session exists (login may still be going on). Fails if no
    /// session appears within `timeout`.
    fn open(&self, connection: &str, timeout: Duration) -> Result<String, String>;
    fn close(&self, session: &str) -> Result<(), String>;
    /// The text on the terminal screen right now (what the user sees, not the
    /// stream of output), one line per row.
    fn screen(&self, session: &str) -> Result<String, String>;
    /// Ask the person at the keyboard. `None`: they cancelled, or the script was stopped.
    fn ask(&self, run: u64, prompt: &str, secret: bool) -> Option<String>;
}

/// Pick a saved connection by id, path ("Lab/spine01") or name; case doesn't matter.
pub fn resolve<'a>(all: &'a [ConnectionInfo], query: &str) -> Result<&'a ConnectionInfo, String> {
    let q = query.trim();
    if let Some(c) = all.iter().find(|c| c.id == q) {
        return Ok(c);
    }
    let same = |a: &str| a.eq_ignore_ascii_case(q);
    let by_path: Vec<_> = all.iter().filter(|c| same(&c.path)).collect();
    if let [one] = by_path[..] {
        return Ok(one);
    }
    let by_name: Vec<_> = all.iter().filter(|c| same(&c.name)).collect();
    match by_name[..] {
        [one] => Ok(one),
        [] if by_path.len() > 1 => Err(ambiguous(q, &by_path)),
        [] => Err(format!("no saved connection called {q:?}")),
        _ => Err(ambiguous(q, &by_name)),
    }
}

fn ambiguous(q: &str, matches: &[&ConnectionInfo]) -> String {
    let paths: Vec<_> = matches.iter().map(|c| c.path.as_str()).collect();
    format!(
        "{q:?} matches several connections ({}); use the full path",
        paths.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(id: &str, path: &str) -> ConnectionInfo {
        ConnectionInfo {
            id: id.into(),
            name: path.rsplit('/').next().unwrap().into(),
            path: path.into(),
            kind: "ssh".into(),
            endpoint: "10.0.0.1:22".into(),
        }
    }

    #[test]
    fn connections_resolve_by_id_path_or_name() {
        let all = [
            conn("a1", "Lab/spine01"),
            conn("b2", "Lab/leaf01"),
            conn("c3", "Prod/leaf01"),
        ];
        assert_eq!(resolve(&all, "a1").unwrap().id, "a1");
        assert_eq!(resolve(&all, "lab/SPINE01").unwrap().id, "a1");
        assert_eq!(resolve(&all, "spine01").unwrap().id, "a1");
        assert_eq!(resolve(&all, "Prod/leaf01").unwrap().id, "c3");
        let e = resolve(&all, "leaf01").unwrap_err();
        assert!(e.contains("Lab/leaf01") && e.contains("Prod/leaf01"), "{e}");
        assert!(resolve(&all, "nope").unwrap_err().contains("no saved connection"));
    }
}
