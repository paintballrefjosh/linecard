//! Python scripts: the app's side of `linecard-script`.
//!
//! Every terminal session registers here with a [`Tap`] holding its recent
//! output. A running script reaches those sessions through [`AppHost`]; the
//! Scripts window starts and stops runs and answers the script's questions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use linecard_core_model::ConnectionKind;
use linecard_script::{python, run, ConnectionInfo, Event, Host, Options, Run, SessionInfo, Tap};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::launch::Launch;
use crate::{err, AppState, CmdResult};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// An open terminal session, as scripts see it.
pub struct Entry {
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    pub connection: Option<String>,
    pub tap: Arc<Tap>,
}

/// A script that is running.
struct Live {
    run: OnceLock<Run>,
    ui: Channel<ScriptEventDto>,
    /// The question being asked, if any: its number and where the answer goes.
    ask: Mutex<Option<(u64, mpsc::Sender<Option<String>>)>>,
    asks: AtomicU64,
}

impl Live {
    /// Stop waiting for an answer (the script was stopped or has ended).
    fn cancel_ask(&self) {
        lock(&self.ask).take();
    }
}

#[derive(Default)]
pub struct Scripting {
    sessions: Mutex<HashMap<String, Entry>>,
    /// The terminal tab used last.
    active: Mutex<Option<String>>,
    runs: Mutex<HashMap<u64, Arc<Live>>>,
    /// Screen reads waiting for the window to answer.
    screens: Mutex<HashMap<u64, mpsc::Sender<Option<String>>>>,
    next_screen: AtomicU64,
}

impl Scripting {
    pub fn register(&self, id: &str, entry: Entry) {
        lock(&self.sessions).insert(id.to_string(), entry);
    }

    /// The session is gone; scripts waiting on it find out.
    pub fn unregister(&self, id: &str) {
        if let Some(e) = lock(&self.sessions).remove(id) {
            e.tap.end(None);
        }
        let mut active = lock(&self.active);
        if active.as_deref() == Some(id) {
            *active = None;
        }
    }
}

pub fn kind_name(kind: ConnectionKind) -> String {
    format!("{kind:?}").to_lowercase()
}

/// What the Scripts window is told about a run.
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum ScriptEventDto {
    Output {
        stderr: bool,
        text: String,
    },
    /// The script asks for text; answer with `script_answer`.
    Ask {
        id: u64,
        prompt: String,
        secret: bool,
    },
    Finished {
        code: Option<i32>,
        stopped: bool,
    },
}

pub struct AppHost(pub AppHandle);

impl AppHost {
    fn state(&self) -> State<'_, AppState> {
        self.0.state::<AppState>()
    }
}

impl Host for AppHost {
    fn sessions(&self) -> Vec<SessionInfo> {
        let st = self.state();
        let active = lock(&st.scripting.active).clone();
        let mut all: Vec<_> = lock(&st.scripting.sessions)
            .iter()
            .filter(|(_, e)| !e.tap.is_ended())
            .map(|(id, e)| SessionInfo {
                id: id.clone(),
                name: e.name.clone(),
                kind: e.kind.clone(),
                endpoint: e.endpoint.clone(),
                connection: e.connection.clone(),
                active: active.as_deref() == Some(id),
                connected: e.tap.is_connected(),
            })
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        all
    }

    fn connections(&self) -> Vec<ConnectionInfo> {
        let st = self.state();
        let Ok(lib) = st.lib.lock() else { return vec![] };
        let tree = lib.tree();
        let mut all: Vec<_> = tree
            .connections()
            .filter(|c| {
                matches!(
                    c.kind,
                    ConnectionKind::Ssh | ConnectionKind::Telnet | ConnectionKind::Serial
                )
            })
            .map(|c| {
                let folder = tree.path(c.folder.as_deref()).unwrap_or_default();
                let folder = folder.trim_matches('/');
                ConnectionInfo {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    path: if folder.is_empty() {
                        c.name.clone()
                    } else {
                        format!("{folder}/{}", c.name)
                    },
                    kind: kind_name(c.kind),
                    endpoint: c.endpoint.label(),
                }
            })
            .collect();
        all.sort_by_key(|a| a.path.to_lowercase());
        all
    }

    fn tap(&self, session: &str) -> Option<Arc<Tap>> {
        lock(&self.state().scripting.sessions)
            .get(session)
            .map(|e| e.tap.clone())
    }

    fn write(&self, session: &str, data: &[u8]) -> Result<(), String> {
        let st = self.state();
        let mut sessions = lock(&st.sessions);
        let s = sessions.get_mut(session).ok_or("that session is closed")?;
        s.write(data).map_err(err)
    }

    fn open(&self, connection: &str, timeout: Duration) -> Result<String, String> {
        let st = self.state();
        let known = |st: &AppState| -> Vec<String> {
            lock(&st.scripting.sessions)
                .iter()
                .filter(|(_, e)| e.connection.as_deref() == Some(connection))
                .map(|(id, _)| id.clone())
                .collect()
        };
        let before = known(&st);
        // The window opens a tab for it, exactly as a double-click would.
        self.0
            .emit(
                "launch",
                Launch::Saved {
                    id: connection.to_string(),
                },
            )
            .map_err(err)?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(id) = known(&st).into_iter().find(|id| !before.contains(id)) {
                return Ok(id);
            }
            if Instant::now() >= deadline {
                return Err("the tab did not open in time".into());
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    fn close(&self, session: &str) -> Result<(), String> {
        let st = self.state();
        let closed = lock(&st.sessions).remove(session);
        match closed {
            Some(mut s) => {
                let _ = s.close();
                st.scripting.unregister(session);
                Ok(())
            }
            None => Err("that session is closed".into()),
        }
    }

    fn screen(&self, session: &str) -> Result<String, String> {
        let st = self.state();
        let (tx, rx) = mpsc::channel();
        let id = st.scripting.next_screen.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&st.scripting.screens).insert(id, tx);
        let sent = self.0.emit(
            "script-screen",
            ScreenRequest {
                id,
                session: session.to_string(),
            },
        );
        let answer = match sent {
            Ok(()) => rx.recv_timeout(Duration::from_secs(5)).ok().flatten(),
            Err(_) => None,
        };
        lock(&st.scripting.screens).remove(&id);
        answer.ok_or_else(|| "that terminal can't be read right now".to_string())
    }

    fn ask(&self, run: u64, prompt: &str, secret: bool) -> Option<String> {
        let live = lock(&self.state().scripting.runs).get(&run)?.clone();
        let (tx, rx) = mpsc::channel();
        let id = live.asks.fetch_add(1, Ordering::Relaxed) + 1;
        *lock(&live.ask) = Some((id, tx));
        live.ui
            .send(ScriptEventDto::Ask {
                id,
                prompt: prompt.to_string(),
                secret,
            })
            .ok()?;
        // A dropped sender (script stopped) ends the wait with None.
        rx.recv().ok().flatten()
    }
}

#[derive(Clone, Serialize)]
struct ScreenRequest {
    id: u64,
    session: String,
}

// ---- commands ----------------------------------------------------------------

#[derive(Serialize)]
pub struct ScriptStatusDto {
    /// e.g. "3.11.9"; none if no usable Python was found.
    version: Option<String>,
    /// Why not, when there's none.
    problem: Option<String>,
    folder: String,
}

#[derive(Serialize)]
pub struct ScriptFileDto {
    name: String,
    path: String,
    modified_ms: u64,
}

fn scripts_dir(state: &AppState) -> PathBuf {
    state.data_dir.join("scripts")
}

/// The Python path from Settings (empty: find one).
fn python_setting(state: &AppState) -> Option<String> {
    let raw = state.lib.lock().ok()?.local_setting(crate::PREFERENCES_KEY).ok()??;
    let prefs: serde_json::Value = serde_json::from_str(&raw).ok()?;
    prefs.get("pythonPath")?.as_str().map(str::to_string)
}

async fn find_python(custom: Option<String>) -> Result<python::Python, String> {
    tauri::async_runtime::spawn_blocking(move || python::find(custom.as_deref()))
        .await
        .map_err(err)?
}

/// Whether scripts can run, and where they live.
#[tauri::command]
pub async fn script_status(state: State<'_, AppState>) -> CmdResult<ScriptStatusDto> {
    let folder = scripts_dir(&state).display().to_string();
    let found = find_python(python_setting(&state)).await;
    Ok(match found {
        Ok(p) => ScriptStatusDto {
            version: Some(p.version),
            problem: None,
            folder,
        },
        Err(e) => ScriptStatusDto {
            version: None,
            problem: Some(e),
            folder,
        },
    })
}

/// The `.py` files in the scripts folder, newest first.
#[tauri::command]
pub fn script_list(state: State<AppState>) -> Vec<ScriptFileDto> {
    let Ok(entries) = std::fs::read_dir(scripts_dir(&state)) else {
        return vec![];
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "py") && e.path().is_file())
        .map(|e| ScriptFileDto {
            name: e.path().file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            path: e.path().display().to_string(),
            modified_ms: e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64),
        })
        .collect();
    files.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then_with(|| a.name.cmp(&b.name)));
    files
}

const TEMPLATE: &str = r##"# A Linecard script. Run it from the Scripts window.
# Reference: docs/scripting.md in the Linecard repository.
import linecard as lc

s = lc.active()              # the terminal tab you used last
s.send_line("show version")  # type a command and press Enter
s.expect("#", timeout=10)    # wait for the prompt
print(s.before)              # what the device printed meanwhile
"##;

/// Make a new script from a template; returns its path.
#[tauri::command]
pub fn script_new(state: State<AppState>, name: String) -> CmdResult<String> {
    let stem: String = name
        .trim()
        .trim_end_matches(".py")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = stem.trim();
    if stem.is_empty() {
        return Err("give the script a name".into());
    }
    let dir = scripts_dir(&state);
    std::fs::create_dir_all(&dir).map_err(err)?;
    let path = dir.join(format!("{stem}.py"));
    if path.exists() {
        return Err(format!("there is already a script called {stem}"));
    }
    std::fs::write(&path, TEMPLATE).map_err(err)?;
    Ok(path.display().to_string())
}

/// Show the scripts folder in the file manager.
#[tauri::command]
pub fn open_scripts_folder(state: State<AppState>) -> CmdResult<()> {
    let dir = scripts_dir(&state);
    std::fs::create_dir_all(&dir).map_err(err)?;
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(err)
}

/// Start a script. Output, questions and the end arrive on `on_event`.
#[tauri::command]
pub async fn script_run(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    on_event: Channel<ScriptEventDto>,
) -> CmdResult<u64> {
    let python = find_python(python_setting(&state)).await?;
    let script = PathBuf::from(&path);
    if !script.is_file() {
        return Err(format!("{path} doesn't exist"));
    }
    let id = run::next_id();
    let live = Arc::new(Live {
        run: OnceLock::new(),
        ui: on_event,
        ask: Mutex::new(None),
        asks: AtomicU64::new(0),
    });
    lock(&state.scripting.runs).insert(id, live.clone());
    crate::debuglog::write("script", &format!("run {id}: {path}"));

    let opts = Options {
        python,
        script,
        args: vec![],
        lib_dir: state.data_dir.join("script-lib"),
    };
    let (ended, handle) = (live.clone(), app.clone());
    let started = Run::start(id, Arc::new(AppHost(app)), &opts, move |e| match e {
        Event::Output { stderr, text } => {
            let _ = ended.ui.send(ScriptEventDto::Output { stderr, text });
        }
        Event::Finished(o) => {
            ended.cancel_ask();
            lock(&handle.state::<AppState>().scripting.runs).remove(&id);
            let _ = ended.ui.send(ScriptEventDto::Finished {
                code: o.code,
                stopped: o.stopped,
            });
        }
    });
    match started {
        Ok(r) => {
            let _ = live.run.set(r);
            Ok(id)
        }
        Err(e) => {
            lock(&state.scripting.runs).remove(&id);
            Err(format!("could not start Python: {e}"))
        }
    }
}

#[tauri::command]
pub fn script_stop(state: State<AppState>, run_id: u64) {
    let live = lock(&state.scripting.runs).get(&run_id).cloned();
    if let Some(live) = live {
        if let Some(r) = live.run.get() {
            r.stop();
        }
        live.cancel_ask();
    }
}

/// The answer to a script's question (`None`: cancelled).
#[tauri::command]
pub fn script_answer(state: State<AppState>, run_id: u64, ask_id: u64, value: Option<String>) {
    let live = lock(&state.scripting.runs).get(&run_id).cloned();
    if let Some(live) = live {
        let mut slot = lock(&live.ask);
        if slot.as_ref().is_some_and(|(id, _)| *id == ask_id) {
            if let Some((_, tx)) = slot.take() {
                let _ = tx.send(value);
            }
        }
    }
}

/// The terminal tab being used now; scripts' `active()` finds it.
#[tauri::command]
pub fn session_focus(state: State<AppState>, session_id: String) {
    if lock(&state.scripting.sessions).contains_key(&session_id) {
        *lock(&state.scripting.active) = Some(session_id);
    }
}

/// The window's answer to a screen read (`None`: no such terminal).
#[tauri::command]
pub fn script_screen_reply(state: State<AppState>, id: u64, text: Option<String>) {
    if let Some(tx) = lock(&state.scripting.screens).get(&id) {
        let _ = tx.send(text);
    }
}
