//! Graphical sessions (VNC, RDP) in a tab.
//!
//! Screen updates go to the UI as binary messages: an 8-byte header
//! (x, y, width, height as little-endian u16) followed by RGBA pixels, which
//! the tab paints onto a canvas. Everything else is JSON, shaped like the
//! terminal's events so the same login and certificate prompts work.

use std::sync::{Arc, Mutex};

use base64::Engine;
use linecard_core_model::{ConnectionKind, Endpoint};
use linecard_proto::gfx::{CursorShape, GfxEvent, GfxSession};
use linecard_proto::{SessionEvent, TerminalTarget};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::dto::{PromptReplyDto, SessionEventDto};
use crate::flow::Flow;
use crate::{err, network_target, new_id, parse_endpoint, AppState, CmdResult};

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum GfxEventDto {
    Connected {
        width: u16,
        height: u16,
        name: String,
    },
    Resized {
        width: u16,
        height: u16,
    },
    Bell,
    Clipboard {
        text: String,
    },
    /// `shape`: "default", "hidden" or "image" (then the rest is filled in).
    #[serde(rename_all = "camelCase")]
    Cursor {
        shape: &'static str,
        width: u16,
        height: u16,
        hot_x: u16,
        hot_y: u16,
        /// RGBA pixels, base64.
        rgba: String,
    },
    Disconnected {
        reason: Option<String>,
    },
}

pub(crate) fn send_json(ch: &Channel<InvokeResponseBody>, v: &impl Serialize) {
    if let Ok(s) = serde_json::to_string(v) {
        let _ = ch.send(InvokeResponseBody::Json(s));
    }
}

/// A frame message: x, y, width, height (u16 little endian), then RGBA.
fn frame_msg(x: u16, y: u16, width: u16, height: u16, rgba: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(8 + rgba.len());
    for v in [x, y, width, height] {
        msg.extend(v.to_le_bytes());
    }
    msg.extend(rgba);
    msg
}

/// The remote screen as the server last drew it. When the window falls
/// behind (see `flow`), frames stop going to it and the area they covered is
/// remembered; once it catches up that area is sent from here, so the
/// screen is right again without asking the server.
#[derive(Default)]
struct Screen {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
    /// Area not yet sent to the window: x0, y0, x1, y1 (end exclusive).
    dirty: Option<(u16, u16, u16, u16)>,
}

impl Screen {
    fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.rgba = vec![0; usize::from(width) * usize::from(height) * 4];
        self.dirty = None;
    }

    /// The part of a frame that's on the screen, as x0, y0, x1, y1.
    fn clip(&self, x: u16, y: u16, w: u16, h: u16) -> Option<(u16, u16, u16, u16)> {
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        (x < x1 && y < y1).then_some((x, y, x1, y1))
    }

    fn paint(&mut self, x: u16, y: u16, w: u16, h: u16, rgba: &[u8]) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else {
            return;
        };
        if rgba.len() < usize::from(w) * usize::from(h) * 4 {
            return;
        }
        let n = usize::from(x1 - x0) * 4;
        for row in y0..y1 {
            let from = usize::from(row - y) * usize::from(w) * 4;
            let to = (usize::from(row) * usize::from(self.width) + usize::from(x0)) * 4;
            self.rgba[to..to + n].copy_from_slice(&rgba[from..from + n]);
        }
    }

    fn mark(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let Some(r) = self.clip(x, y, w, h) else { return };
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => (d.0.min(r.0), d.1.min(r.1), d.2.max(r.2), d.3.max(r.3)),
        });
    }

    /// The area not yet sent, as a frame message.
    fn take_dirty(&mut self) -> Option<Vec<u8>> {
        let (x0, y0, x1, y1) = self.dirty.take()?;
        let n = usize::from(x1 - x0) * 4;
        let mut rgba = Vec::with_capacity(n * usize::from(y1 - y0));
        for row in y0..y1 {
            let at = (usize::from(row) * usize::from(self.width) + usize::from(x0)) * 4;
            rgba.extend_from_slice(&self.rgba[at..at + n]);
        }
        Some(frame_msg(x0, y0, x1 - x0, y1 - y0, &rgba))
    }
}

/// Once the window catches up, send the area of the screen it missed.
fn resume_screen(flow: &Flow, screen: &Arc<Mutex<Screen>>) {
    let s = screen.clone();
    flow.on_resume(move |f| {
        let Ok(mut s) = s.lock() else { return };
        let dirty = s.dirty;
        if let Some(msg) = s.take_dirty() {
            if !f.catch_up(msg) {
                s.dirty = dirty;
            }
        }
    });
}

/// Sound waiting to be sent: waves come every few milliseconds and each
/// message costs the window time, so they go out about 100 ms at a time.
#[derive(Default)]
struct AudioBatch {
    channels: u16,
    rate: u32,
    pcm: Vec<u8>,
}

impl AudioBatch {
    /// Binary like a frame, told apart by 0xFFFF where the frame's x would be:
    /// marker, channels, rate (u16, u16, u32 little endian), then 16-bit samples.
    /// Sound the window can't take now (it's behind) is dropped, not queued.
    fn flush(&mut self, flow: &Flow) {
        if self.pcm.is_empty() {
            return;
        }
        let mut msg = Vec::with_capacity(8 + self.pcm.len());
        msg.extend(0xFFFFu16.to_le_bytes());
        msg.extend(self.channels.to_le_bytes());
        msg.extend(self.rate.to_le_bytes());
        msg.append(&mut self.pcm);
        flow.send(msg);
    }

    fn add(&mut self, flow: &Flow, channels: u16, rate: u32, pcm: Vec<u8>) {
        if (self.channels, self.rate) != (channels, rate) {
            self.flush(flow);
            (self.channels, self.rate) = (channels, rate);
        }
        self.pcm.extend(pcm);
        if self.pcm.len() >= usize::from(channels) * rate as usize * 2 / 10 {
            self.flush(flow);
        }
    }
}

struct Out {
    ch: Channel<InvokeResponseBody>,
    flow: Arc<Flow>,
    screen: Arc<Mutex<Screen>>,
    audio: AudioBatch,
}

fn forward(out: &mut Out, who: &str, e: GfxEvent) {
    let ch = &out.ch;
    match e {
        GfxEvent::Frame {
            x,
            y,
            width,
            height,
            rgba,
        } => {
            let Ok(mut screen) = out.screen.lock() else { return };
            screen.paint(x, y, width, height, &rgba);
            if !out.flow.send(frame_msg(x, y, width, height, &rgba)) {
                screen.mark(x, y, width, height);
            }
        }
        GfxEvent::Connected { width, height, name } => {
            if let Ok(mut s) = out.screen.lock() {
                s.resize(width, height);
            }
            send_json(ch, &GfxEventDto::Connected { width, height, name })
        }
        GfxEvent::Resized { width, height } => {
            if let Ok(mut s) = out.screen.lock() {
                s.resize(width, height);
            }
            send_json(ch, &GfxEventDto::Resized { width, height })
        }
        GfxEvent::Bell => send_json(ch, &GfxEventDto::Bell),
        GfxEvent::Clipboard(text) => send_json(ch, &GfxEventDto::Clipboard { text }),
        GfxEvent::Audio { channels, rate, pcm } => out.audio.add(&out.flow, channels, rate, pcm),
        GfxEvent::Cursor(shape) => {
            let (shape, width, height, hot_x, hot_y, rgba) = match shape {
                CursorShape::Default => ("default", 0, 0, 0, 0, String::new()),
                CursorShape::Hidden => ("hidden", 0, 0, 0, 0, String::new()),
                CursorShape::Image {
                    width,
                    height,
                    hot_x,
                    hot_y,
                    rgba,
                } => (
                    "image",
                    width,
                    height,
                    hot_x,
                    hot_y,
                    base64::engine::general_purpose::STANDARD.encode(rgba),
                ),
            };
            send_json(
                ch,
                &GfxEventDto::Cursor {
                    shape,
                    width,
                    height,
                    hot_x,
                    hot_y,
                    rgba,
                },
            )
        }
        GfxEvent::Disconnected { reason } => {
            if let Some(r) = &reason {
                crate::debuglog::write("remote", &format!("{who}: {r}"));
            }
            send_json(ch, &GfxEventDto::Disconnected { reason })
        }
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
    let credentials = state.ssh.credentials.clone();
    let who = format!("{kind:?} {endpoint}");
    let flow = Flow::new(on_event.clone(), crate::flow::SCREEN_LIMIT);
    let screen = Arc::new(Mutex::new(Screen::default()));
    resume_screen(&flow, &screen);
    let mut out = Out {
        ch: on_event,
        flow,
        screen,
        audio: AudioBatch::default(),
    };
    let sink: linecard_proto::gfx::GfxSink = Box::new(move |e| forward(&mut out, &who, e));
    let session: Box<dyn GfxSession> = match kind {
        ConnectionKind::Vnc => Box::new(
            linecard_proto::vnc::VncSession::connect_with(
                &target,
                credentials,
                state.ssh.clone(),
                state.data_dir.join("known_rdp_certs"),
                sink,
            )
            .map_err(err)?,
        ),
        ConnectionKind::Rdp => {
            let opts = linecard_proto::rdp::RdpOptions {
                credentials,
                known_certs: state.data_dir.join("known_rdp_certs"),
                ssh: state.ssh.clone(),
                share: crate::shares::get(&state.data_dir, &connection_id).map(|s| linecard_proto::rdp::ShareFolder {
                    path: s.path.into(),
                    writable: s.writable,
                }),
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
pub fn gfx_clipboard(state: State<AppState>, session_id: String, text: String) -> CmdResult<()> {
    with(&state, &session_id, |s| s.clipboard(&text))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn px(screen: &Screen, x: u16, y: u16) -> u8 {
        screen.rgba[(usize::from(y) * usize::from(screen.width) + usize::from(x)) * 4]
    }

    #[test]
    fn screen_keeps_frames_and_sends_the_missed_area() {
        let mut s = Screen::default();
        s.resize(10, 10);
        // 2×2 of 7s at (1, 1), and 3×1 of 9s at (5, 8): both missed.
        s.paint(1, 1, 2, 2, &[7; 16]);
        s.mark(1, 1, 2, 2);
        s.paint(5, 8, 3, 1, &[9; 12]);
        s.mark(5, 8, 3, 1);
        assert_eq!((px(&s, 2, 2), px(&s, 7, 8), px(&s, 0, 0)), (7, 9, 0));

        let msg = s.take_dirty().unwrap();
        let head: Vec<u16> = msg[..8].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(head, [1, 1, 7, 8], "one area covering both");
        assert_eq!(msg.len(), 8 + 7 * 8 * 4);
        // Row 1, first column of the area is a 7; row 8 at x = 5 is a 9.
        assert_eq!(msg[8], 7);
        assert_eq!(msg[8 + (7 * 7 + 4) * 4], 9);
        assert!(s.take_dirty().is_none());
    }

    #[test]
    fn frames_past_the_edge_are_clipped() {
        let mut s = Screen::default();
        s.resize(4, 4);
        s.paint(3, 3, 2, 2, &[5; 16]);
        s.mark(3, 3, 2, 2);
        assert_eq!(px(&s, 3, 3), 5);
        assert_eq!(&s.take_dirty().unwrap()[..8], &[3, 0, 3, 0, 1, 0, 1, 0]);
        // Before the size is known nothing is kept, and nothing breaks.
        let mut empty = Screen::default();
        empty.paint(0, 0, 1, 1, &[1; 4]);
        empty.mark(0, 0, 1, 1);
        assert!(empty.take_dirty().is_none());
    }
}
