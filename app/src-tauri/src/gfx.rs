//! Graphical sessions (VNC, RDP) in a tab.
//!
//! Screen updates go to the UI as binary messages: an 8-byte header
//! (x, y, width, height as little-endian u16) followed by RGBA pixels, which
//! the tab paints onto a canvas. Everything else is JSON, shaped like the
//! terminal's events so the same login and certificate prompts work.

use linecard_core_model::{ConnectionKind, Endpoint};
use linecard_proto::gfx::{GfxEvent, GfxSession};
use linecard_proto::{SessionEvent, TerminalTarget};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::dto::{PromptReplyDto, SessionEventDto};
use crate::{err, network_target, new_id, parse_endpoint, AppState, CmdResult};

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum GfxEventDto {
    Connected { width: u16, height: u16, name: String },
    Resized { width: u16, height: u16 },
    Bell,
    Clipboard { text: String },
    Disconnected { reason: Option<String> },
}

fn send_json(ch: &Channel<InvokeResponseBody>, v: &impl Serialize) {
    if let Ok(s) = serde_json::to_string(v) {
        let _ = ch.send(InvokeResponseBody::Json(s));
    }
}

fn forward(ch: &Channel<InvokeResponseBody>, e: GfxEvent) {
    match e {
        GfxEvent::Frame {
            x,
            y,
            width,
            height,
            rgba,
        } => {
            let mut msg = Vec::with_capacity(8 + rgba.len());
            for v in [x, y, width, height] {
                msg.extend(v.to_le_bytes());
            }
            msg.extend(rgba);
            let _ = ch.send(InvokeResponseBody::Raw(msg));
        }
        GfxEvent::Connected { width, height, name } => send_json(ch, &GfxEventDto::Connected { width, height, name }),
        GfxEvent::Resized { width, height } => send_json(ch, &GfxEventDto::Resized { width, height }),
        GfxEvent::Bell => send_json(ch, &GfxEventDto::Bell),
        GfxEvent::Clipboard(text) => send_json(ch, &GfxEventDto::Clipboard { text }),
        GfxEvent::Disconnected { reason } => send_json(ch, &GfxEventDto::Disconnected { reason }),
        GfxEvent::Auth(p) => send_json(ch, &SessionEventDto::from(SessionEvent::Auth(p))),
        GfxEvent::HostKey(p) => send_json(ch, &SessionEventDto::from(SessionEvent::HostKey(p))),
    }
}

/// Open a VNC or RDP session; `width`×`height` is the tab's size (RDP asks
/// the server for it).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn open_gfx_session(
    state: State<'_, AppState>,
    connection_id: String,
    kind: ConnectionKind,
    endpoint: String,
    width: u16,
    height: u16,
    on_event: Channel<InvokeResponseBody>,
) -> CmdResult<String> {
    let target: TerminalTarget = {
        let lib = state.lib.lock().map_err(err)?;
        let tree = lib.tree();
        match tree.connection(&connection_id) {
            Ok(c) if matches!(c.endpoint, Endpoint::Network { .. }) => network_target(tree, &c.id, 0)?,
            _ => {
                let (user, host, port) = parse_endpoint(&endpoint, kind).ok_or("could not read host and port")?;
                let mut t = TerminalTarget::new(host, port);
                t.username = user;
                t
            }
        }
    };
    if target.jump.is_some() {
        return Err(
            "VNC and RDP can't go through an SSH jump host yet; set the jump host to None for this connection".into(),
        );
    }
    let credentials = state.ssh.credentials.clone();
    let sink: linecard_proto::gfx::GfxSink = Box::new(move |e| forward(&on_event, e));
    let session: Box<dyn GfxSession> = match kind {
        ConnectionKind::Vnc => {
            Box::new(linecard_proto::vnc::VncSession::connect(&target, credentials, sink).map_err(err)?)
        }
        ConnectionKind::Rdp => {
            let opts = linecard_proto::rdp::RdpOptions {
                credentials,
                known_certs: state.data_dir.join("known_rdp_certs"),
            };
            Box::new(linecard_proto::rdp::RdpSession::connect(&target, width, height, opts, sink).map_err(err)?)
        }
        other => return Err(format!("{other:?} isn't a graphical connection")),
    };
    let id = new_id();
    state.gfx.lock().map_err(err)?.insert(id.clone(), session);
    Ok(id)
}

fn with<T>(state: &AppState, id: &str, f: impl FnOnce(&mut dyn GfxSession) -> std::io::Result<T>) -> CmdResult<T> {
    let mut all = state.gfx.lock().map_err(err)?;
    let s = all.get_mut(id).ok_or("session closed")?;
    f(s.as_mut()).map_err(err)
}

#[tauri::command]
pub fn gfx_pointer(state: State<AppState>, session_id: String, x: u16, y: u16, buttons: u8) -> CmdResult<()> {
    with(&state, &session_id, |s| s.pointer(x, y, buttons))
}

#[tauri::command]
pub fn gfx_wheel(state: State<AppState>, session_id: String, x: u16, y: u16, dx: i16, dy: i16) -> CmdResult<()> {
    with(&state, &session_id, |s| s.wheel(x, y, dx, dy))
}

#[tauri::command]
pub fn gfx_key(state: State<AppState>, session_id: String, code: String, key: String, down: bool) -> CmdResult<()> {
    with(&state, &session_id, |s| s.key(&code, &key, down))
}

#[tauri::command]
pub fn gfx_text(state: State<AppState>, session_id: String, text: String) -> CmdResult<()> {
    with(&state, &session_id, |s| s.text(&text))
}

#[tauri::command]
pub fn gfx_reply(state: State<AppState>, session_id: String, reply: PromptReplyDto) -> CmdResult<()> {
    with(&state, &session_id, |s| s.reply(reply.into()))
}

#[tauri::command]
pub fn gfx_resize(state: State<AppState>, session_id: String, width: u16, height: u16) -> CmdResult<()> {
    with(&state, &session_id, |s| s.resize(width, height))
}

#[tauri::command]
pub fn gfx_close(state: State<AppState>, session_id: String) -> CmdResult<()> {
    if let Some(mut s) = state.gfx.lock().map_err(err)?.remove(&session_id) {
        s.close().map_err(err)?;
    }
    Ok(())
}
