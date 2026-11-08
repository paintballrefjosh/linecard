//! Opening sessions from outside the app: command-line arguments and
//! `ssh://` / `telnet://` links.
//!
//! ```text
//! linecard spine01                       a saved connection, by name
//! linecard "DC1 Fort Worth/Spine/spine01"  …or by its path in the tree
//! linecard ssh admin@10.0.0.1 -p 2222    quick connect (same syntax as the toolbar)
//! linecard telnet cs1 2005
//! linecard serial COM3 9600
//! linecard ssh://admin@10.0.0.1:2222     a link
//! linecard run versions.py               run a script (a path, or the name of one in the scripts folder)
//! ```
//!
//! A second launch hands its arguments to the running window (single
//! instance), so links and the command line open tabs there.

use linecard_core_model::Tree;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{AppState, CmdResult};

/// What to open, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Launch {
    /// A saved connection.
    Saved {
        id: String,
    },
    /// Quick-connect text, e.g. "ssh admin@10.0.0.1:2222".
    Quick {
        text: String,
    },
    /// Run a Python script (`linecard run script.py`); `path` is absolute.
    Script {
        path: String,
    },
    Error {
        message: String,
    },
}

/// Arguments after the program name. Flags the app doesn't know (from the
/// OS or WebView2) are ignored.
pub fn parse(args: &[String], tree: &Tree) -> Option<Launch> {
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.is_empty()).collect();
    let first = *args.first()?;
    if first.eq_ignore_ascii_case("run") {
        return Some(match args.get(1) {
            Some(p) => Launch::Script { path: p.to_string() },
            None => Launch::Error {
                message: "run: which script?".into(),
            },
        });
    }
    if let Some(l) = parse_url(first) {
        return Some(l);
    }
    if first.starts_with('-') {
        return None;
    }
    match first.to_ascii_lowercase().as_str() {
        "ssh" | "telnet" | "serial" | "rdp" | "vnc" => {
            let kind = first.to_ascii_lowercase();
            let mut port = None;
            let mut rest = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match *a {
                    "-p" => port = it.next().copied(),
                    a if a.starts_with('-') => {}
                    a => rest.push(a),
                }
            }
            let Some(host) = rest.first() else {
                return Some(Launch::Error {
                    message: format!("{kind}: which host?"),
                });
            };
            let text = match (port.or(rest.get(1).copied()), kind.as_str()) {
                (Some(p), "serial") => format!("{host} {p}"),
                (None, "serial") => host.to_string(),
                (Some(p), _) => format!("{kind} {host}:{p}"),
                (None, _) => format!("{kind} {host}"),
            };
            Some(Launch::Quick { text })
        }
        _ => Some(find_saved(first, tree).unwrap_or_else(|| Launch::Quick { text: args.join(" ") })),
    }
}

/// `ssh://[user[;params]@]host[:port][/]`, `telnet://host[:port]`.
fn parse_url(s: &str) -> Option<Launch> {
    let (scheme, rest) = s.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "ssh" && scheme != "telnet" {
        return Some(Launch::Error {
            message: format!("{scheme}:// links aren't supported"),
        });
    }
    let rest = rest.trim_end_matches('/');
    let (user, hostport) = match rest.rsplit_once('@') {
        // RFC draft: "user;fingerprint=…" carries connection parameters.
        Some((u, h)) => (Some(percent_decode(u.split(';').next().unwrap_or(""))), h),
        None => (None, rest),
    };
    if hostport.is_empty() || hostport.contains(['/', ' ']) {
        return Some(Launch::Error {
            message: format!("can't read the address in {s}"),
        });
    }
    let user = user
        .filter(|u| !u.is_empty())
        .map(|u| format!("{u}@"))
        .unwrap_or_default();
    Some(Launch::Quick {
        text: format!("{scheme} {user}{hostport}"),
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A saved connection by tree path ("DC1/Spine/spine01", leading slash
/// optional, any case) or by a name only one connection has.
fn find_saved(query: &str, tree: &Tree) -> Option<Launch> {
    let want = query.trim().trim_matches('/').to_lowercase();
    let path_of = |c: &linecard_core_model::Connection| {
        let folder = tree.path(c.folder.as_deref()).unwrap_or_default();
        format!("{}/{}", folder.trim_matches('/'), c.name)
            .trim_matches('/')
            .to_lowercase()
    };
    if let Some(c) = tree.connections().find(|c| path_of(c) == want) {
        return Some(Launch::Saved { id: c.id.clone() });
    }
    let named: Vec<_> = tree.connections().filter(|c| c.name.to_lowercase() == want).collect();
    match named.as_slice() {
        [one] => Some(Launch::Saved { id: one.id.clone() }),
        [] => None,
        many => Some(Launch::Error {
            message: format!(
                "{} connections are named {query}; use the full path, e.g. \"{}\"",
                many.len(),
                path_of(many[0])
            ),
        }),
    }
}

/// Arguments the app was started with, waiting for the UI to ask.
#[derive(Default)]
pub struct Pending(pub std::sync::Mutex<Vec<String>>);

/// What this launch asked to open (once; later calls get nothing).
#[tauri::command]
pub fn take_launch(state: State<AppState>, pending: State<Pending>) -> CmdResult<Option<Launch>> {
    let args = std::mem::take(&mut *pending.0.lock().map_err(|e| e.to_string())?);
    let lib = state.lib.lock().map_err(|e| e.to_string())?;
    let cwd = std::env::current_dir().unwrap_or_default();
    Ok(parse(&args, lib.tree()).map(|l| resolve_script(l, &cwd, &state.data_dir.join("scripts"))))
}

/// Turn the script named on the command line into a file that exists: a path
/// (relative to where the command ran), or a name from the scripts folder
/// (with or without `.py`).
pub fn resolve_script(l: Launch, cwd: &std::path::Path, scripts: &std::path::Path) -> Launch {
    let Launch::Script { path } = l else { return l };
    let given = std::path::Path::new(&path);
    let candidates = [cwd.join(given), scripts.join(given), scripts.join(format!("{path}.py"))];
    match candidates.into_iter().find(|c| c.is_file()) {
        Some(p) => Launch::Script {
            path: p.display().to_string(),
        },
        None => Launch::Error {
            message: format!("run: no script called {path}"),
        },
    }
}

/// Another launch (command line or link) while the app is running.
pub fn second_instance(app: &AppHandle, argv: Vec<String>, cwd: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    let state = app.state::<AppState>();
    let launch = match state.lib.lock() {
        Ok(lib) => parse(argv.get(1..).unwrap_or_default(), lib.tree())
            .map(|l| resolve_script(l, std::path::Path::new(cwd), &state.data_dir.join("scripts"))),
        Err(_) => None,
    };
    if let Some(l) = launch {
        let _ = app.emit("launch", l);
    }
}

// ---- OS integration (see system.rs) ----------------------------------------------

/// Whether ssh:// and telnet:// links open in Linecard; `None` where the
/// app can't change that.
#[tauri::command]
pub fn link_handler_status() -> Option<bool> {
    crate::system::links_on()
}

#[tauri::command]
pub fn set_link_handler(on: bool) -> CmdResult<()> {
    crate::system::set_links(on)
}

#[tauri::command]
pub fn cli_path_status() -> crate::system::CliPathDto {
    crate::system::cli_path()
}

#[tauri::command]
pub fn set_cli_path(on: bool) -> CmdResult<()> {
    crate::system::set_cli_path(on)
}

/// Links handed over by macOS (it doesn't pass them as arguments).
#[cfg(target_os = "macos")]
pub fn opened_urls(app: &AppHandle, urls: Vec<String>) {
    let mut argv = vec![String::new()];
    argv.extend(urls);
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    second_instance(app, argv, &cwd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use linecard_core_model::{Connection, ConnectionKind, Endpoint, Folder, Settings};

    #[test]
    fn run_names_a_script_by_path_or_by_name_in_the_scripts_folder() {
        let dir = std::env::temp_dir().join(format!("lc-run-{}", std::process::id()));
        let (cwd, scripts) = (dir.join("cwd"), dir.join("scripts"));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(cwd.join("here.py"), "").unwrap();
        std::fs::write(scripts.join("versions.py"), "").unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let go = |a: &[&str]| resolve_script(parse(&args(a), &tree()).unwrap(), &cwd, &scripts);
        let path = |l: Launch| match l {
            Launch::Script { path } => path,
            other => panic!("{other:?}"),
        };
        assert!(path(go(&["run", "here.py"])).ends_with("here.py"));
        assert!(path(go(&["run", "versions"])).contains("scripts"));
        assert!(path(go(&["run", "versions.py"])).contains("scripts"));
        assert!(matches!(go(&["run", "nope"]), Launch::Error { .. }));
        assert!(matches!(go(&["run"]), Launch::Error { .. }));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tree() -> Tree {
        let mut t = Tree::default();
        let folder = |id: &str, name: &str, parent: Option<&str>| Folder {
            id: id.into(),
            name: name.into(),
            parent: parent.map(Into::into),
            order: 0.0,
            color: None,
            defaults: Settings::default(),
        };
        t.add_folder(folder("dc1", "DC1 Fort Worth", None)).unwrap();
        t.add_folder(folder("spine", "Spine", Some("dc1"))).unwrap();
        t.add_folder(folder("lab", "Lab", None)).unwrap();
        for (id, name, f) in [("s1", "spine01", "spine"), ("r1", "r1", "dc1"), ("r1b", "r1", "lab")] {
            t.add_connection(Connection {
                id: id.into(),
                name: name.into(),
                folder: Some(f.into()),
                order: 0.0,
                kind: ConnectionKind::Ssh,
                endpoint: Endpoint::network(ConnectionKind::Ssh, "x"),
                overrides: Settings::default(),
                notes: String::new(),
                tags: vec![],
                forwards: vec![],
            })
            .unwrap();
        }
        t
    }

    fn launch(args: &[&str]) -> Option<Launch> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&args, &tree())
    }

    fn quick(text: &str) -> Option<Launch> {
        Some(Launch::Quick { text: text.into() })
    }

    #[test]
    fn links() {
        assert_eq!(
            launch(&["ssh://admin@10.0.0.1:2222/"]),
            quick("ssh admin@10.0.0.1:2222")
        );
        assert_eq!(launch(&["SSH://j%2Edoe;fingerprint=x@sw1"]), quick("ssh j.doe@sw1"));
        assert_eq!(launch(&["telnet://cs1:2005"]), quick("telnet cs1:2005"));
        assert!(matches!(launch(&["ftp://x"]), Some(Launch::Error { .. })));
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            launch(&["ssh", "admin@10.0.0.1", "-p", "2222"]),
            quick("ssh admin@10.0.0.1:2222")
        );
        assert_eq!(launch(&["telnet", "cs1", "2005"]), quick("telnet cs1:2005"));
        assert_eq!(launch(&["serial", "COM3", "115200"]), quick("COM3 115200"));
        assert_eq!(launch(&["admin@10.9.9.9"]), quick("admin@10.9.9.9"));
        assert_eq!(launch(&[]), None);
        assert_eq!(launch(&["--some-webview-flag"]), None);
    }

    #[test]
    fn saved_connections_by_name_or_path() {
        let saved = |id: &str| Some(Launch::Saved { id: id.into() });
        assert_eq!(launch(&["spine01"]), saved("s1"));
        assert_eq!(launch(&["/dc1 fort worth/spine/SPINE01"]), saved("s1"));
        assert_eq!(launch(&["Lab/r1"]), saved("r1b"));
        match launch(&["r1"]) {
            Some(Launch::Error { message }) => assert!(message.contains("2 connections"), "{message}"),
            other => panic!("ambiguous name should be an error, got {other:?}"),
        }
    }
}
