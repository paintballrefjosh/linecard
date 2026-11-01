//! SFTP file browsing: one SSH login per browser tab, with the same host-key
//! and password prompts as a terminal.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use linecard_core_model::ConnectionKind;
use linecard_proto::sftp::{join, EntryKind, SftpClient, Transfer};
use linecard_proto::{PromptReply, SessionEvent, TerminalTarget};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::dto::{PromptReplyDto, SessionEventDto};
use crate::{err, network_target, parse_endpoint, AppState, CmdResult};

/// An open (or still logging in) SFTP tab.
pub struct Slot {
    replies: UnboundedSender<PromptReply>,
    client: Option<Arc<SftpClient>>,
    /// Set by `sftp_cancel`; the running transfer stops at its next chunk.
    cancel: Arc<AtomicBool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    name: String,
    /// "file", "dir", "link" or "other".
    kind: &'static str,
    size: u64,
    /// Seconds since the Unix epoch.
    modified: Option<u64>,
    /// Unix permission bits (e.g. 0o644).
    mode: Option<u32>,
}

fn client(state: &AppState, id: &str) -> CmdResult<Arc<SftpClient>> {
    let all = state.sftp.lock().map_err(err)?;
    all.get(id)
        .and_then(|s| s.client.clone())
        .ok_or_else(|| "the SFTP session is closed".to_string())
}

/// Log in to a connection's SSH server for file transfer. `session_id` is
/// chosen by the UI so it can answer prompts while this call is still
/// waiting. Returns the login directory.
#[tauri::command]
pub async fn sftp_open(
    state: State<'_, AppState>,
    session_id: String,
    connection_id: String,
    endpoint: String,
    on_event: Channel<SessionEventDto>,
) -> CmdResult<String> {
    let target: TerminalTarget = {
        let lib = state.lib.lock().map_err(err)?;
        let tree = lib.tree();
        match tree.connection(&connection_id) {
            Ok(c) if c.kind == ConnectionKind::Ssh => network_target(tree, &c.id, 0)?,
            Ok(_) => return Err("SFTP needs an SSH connection".into()),
            Err(_) => {
                let (user, host, port) =
                    parse_endpoint(&endpoint, ConnectionKind::Ssh).ok_or("could not read host and port")?;
                let mut t = TerminalTarget::new(host, port);
                t.username = user;
                t
            }
        }
    };
    let (reply_tx, reply_rx) = unbounded_channel();
    state.sftp.lock().map_err(err)?.insert(
        session_id.clone(),
        Slot {
            replies: reply_tx,
            client: None,
            cancel: Arc::new(AtomicBool::new(false)),
        },
    );
    let sink: linecard_proto::EventSink = Box::new(move |e| match e {
        SessionEvent::HostKey(_) | SessionEvent::Auth(_) => {
            let _ = on_event.send(SessionEventDto::from(e));
        }
        _ => {}
    });
    let replies = Arc::new(tokio::sync::Mutex::new(reply_rx));
    let opened = async {
        let c = SftpClient::connect(&target, &state.ssh, sink, replies).await?;
        let home = c.home().await?;
        Ok::<_, std::io::Error>((c, home))
    }
    .await;
    match opened {
        Ok((c, home)) => {
            if let Some(slot) = state.sftp.lock().map_err(err)?.get_mut(&session_id) {
                slot.client = Some(Arc::new(c));
                Ok(home)
            } else {
                Err("closed".into()) // the tab was closed while logging in
            }
        }
        Err(e) => {
            state.sftp.lock().map_err(err)?.remove(&session_id);
            crate::debuglog::write("sftp", &format!("{}: {e}", target.host));
            Err(e.to_string())
        }
    }
}

/// Answer a host-key or password prompt from `sftp_open`.
#[tauri::command]
pub fn sftp_reply(state: State<AppState>, session_id: String, reply: PromptReplyDto) -> CmdResult<()> {
    let all = state.sftp.lock().map_err(err)?;
    let slot = all.get(&session_id).ok_or("session closed")?;
    slot.replies
        .send(reply.into())
        .map_err(|_| "session closed".to_string())
}

#[tauri::command]
pub async fn sftp_list(state: State<'_, AppState>, session_id: String, path: String) -> CmdResult<Vec<EntryDto>> {
    let c = client(&state, &session_id)?;
    let entries = c.list(&path).await.map_err(err)?;
    Ok(entries
        .into_iter()
        .map(|e| EntryDto {
            name: e.name,
            kind: match e.kind {
                EntryKind::File => "file",
                EntryKind::Dir => "dir",
                EntryKind::Link => "link",
                EntryKind::Other => "other",
            },
            size: e.size,
            modified: e.modified,
            mode: e.mode.map(|m| m & 0o7777),
        })
        .collect())
}

/// The absolute form of a path (`..` resolved), and an error if it can't be listed.
#[tauri::command]
pub async fn sftp_resolve(state: State<'_, AppState>, session_id: String, path: String) -> CmdResult<String> {
    client(&state, &session_id)?.resolve(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_mkdir(state: State<'_, AppState>, session_id: String, path: String) -> CmdResult<()> {
    client(&state, &session_id)?.mkdir(&path).await.map_err(err)
}

#[tauri::command]
pub async fn sftp_rename(state: State<'_, AppState>, session_id: String, from: String, to: String) -> CmdResult<()> {
    client(&state, &session_id)?.rename(&from, &to).await.map_err(err)
}

/// Delete files or folders (folders with everything inside).
#[tauri::command]
pub async fn sftp_remove(state: State<'_, AppState>, session_id: String, paths: Vec<String>) -> CmdResult<()> {
    let c = client(&state, &session_id)?;
    for p in paths {
        c.remove(&p).await.map_err(|e| format!("{p}: {e}"))?;
    }
    Ok(())
}

/// The tab's client and a cleared cancel flag, for a new transfer.
fn for_transfer(state: &AppState, id: &str) -> CmdResult<(Arc<SftpClient>, Arc<AtomicBool>)> {
    let all = state.sftp.lock().map_err(err)?;
    let slot = all.get(id).ok_or("the SFTP session is closed")?;
    let c = slot.client.clone().ok_or("the SFTP session is closed")?;
    slot.cancel.store(false, Ordering::Relaxed);
    Ok((c, slot.cancel.clone()))
}

/// Stop the transfer in progress; what was copied so far stays, and the
/// transfer can be resumed.
#[tauri::command]
pub fn sftp_cancel(state: State<AppState>, session_id: String) -> CmdResult<()> {
    if let Some(slot) = state.sftp.lock().map_err(err)?.get(&session_id) {
        slot.cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Set permission bits on a file or folder (and what is inside it).
#[tauri::command]
pub async fn sftp_chmod(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    mode: u32,
    recursive: bool,
) -> CmdResult<()> {
    client(&state, &session_id)?
        .chmod(&path, mode, recursive)
        .await
        .map_err(|e| format!("{path}: {e}"))
}

/// A progress callback that reports at most every 100 ms.
fn throttled(ch: Channel<u64>) -> impl FnMut(u64) + Send {
    let mut last = Instant::now() - Duration::from_secs(1);
    move |n| {
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            let _ = ch.send(n);
        }
    }
}

/// Copy remote files or folders into the local folder `dest`.
#[tauri::command]
pub async fn sftp_download(
    state: State<'_, AppState>,
    session_id: String,
    remote: Vec<String>,
    dest: String,
    resume: bool,
    on_progress: Channel<u64>,
) -> CmdResult<()> {
    let (c, cancel) = for_transfer(&state, &session_id)?;
    let mut total = 0u64;
    for r in remote {
        let name = r
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("download")
            .to_string();
        let local = PathBuf::from(&dest).join(&name);
        if local.exists() && !resume {
            return Err(format!("{} already exists here", local.display()));
        }
        let before = total;
        let mut report = throttled(on_progress.clone());
        c.download(
            &r,
            &local,
            Transfer {
                resume,
                cancel: &cancel,
            },
            &mut |n| {
                total = before + n;
                report(total)
            },
        )
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    }
    let _ = on_progress.send(total);
    Ok(())
}

/// Copy local files or folders into the remote folder `dir`.
#[tauri::command]
pub async fn sftp_upload(
    state: State<'_, AppState>,
    session_id: String,
    local: Vec<String>,
    dir: String,
    resume: bool,
    on_progress: Channel<u64>,
) -> CmdResult<()> {
    let (c, cancel) = for_transfer(&state, &session_id)?;
    let mut total = 0u64;
    for l in local {
        let path = PathBuf::from(&l);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{l} has no name"))?;
        let before = total;
        let mut report = throttled(on_progress.clone());
        c.upload(
            &path,
            &join(&dir, &name),
            Transfer {
                resume,
                cancel: &cancel,
            },
            &mut |n| {
                total = before + n;
                report(total)
            },
        )
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    }
    let _ = on_progress.send(total);
    Ok(())
}

#[tauri::command]
pub async fn sftp_close(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    let slot = state.sftp.lock().map_err(err)?.remove(&session_id);
    if let Some(slot) = slot {
        // Release a login waiting on a prompt.
        let _ = slot.replies.send(PromptReply::Cancel);
        if let Some(c) = slot.client {
            c.close().await;
        }
    }
    Ok(())
}
