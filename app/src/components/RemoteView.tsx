import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from "react";
import { RotateCw, Volume2, VolumeX } from "lucide-react";
import { getBackend, type GfxHandle } from "../api";
import { PcmPlayer } from "../audio";
import { reconnect } from "../reconnect";
import type { AuthEvent, ConnectionNode, GfxEvent, HostKeyEvent, PromptReply } from "../types";
import { KindIcon, kindLabel } from "./icons";
import { SessionPrompt } from "./SessionPrompt";
import type { SessionStatus } from "./TerminalView";

interface Props {
  conn: ConnectionNode;
  /** The tab this is in, so the tab menu can reconnect it. */
  panelId?: string;
  onStatus(status: SessionStatus): void;
}

/** Browser buttons (left 1, right 2, middle 4) → ours (left 1, middle 2, right 4). */
const buttonsOf = (b: number) => (b & 1) | ((b & 4) >> 1) | ((b & 2) << 1);

/** A VNC or RDP session: the remote screen on a canvas, scaled to fit the
 *  tab, with mouse and keyboard going back to it. */
export function RemoteView({ conn, panelId, onStatus }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const session = useRef<GfxHandle | null>(null);
  const size = useRef({ w: 0, h: 0 });
  const [screen, setScreen] = useState<{ w: number; h: number } | null>(null);
  const [prompt, setPrompt] = useState<HostKeyEvent | AuthEvent | null>(null);
  const [ended, setEnded] = useState<string | null | undefined>(undefined);
  const [attempt, setAttempt] = useState(0);
  const player = useRef(new PcmPlayer());
  const [hasSound, setHasSound] = useState(false);
  const [muted, setMuted] = useState(false);
  /** The clipboard text the server last had (sent or received). */
  const sentClip = useRef("");
  const statusRef = useRef(onStatus);
  statusRef.current = onStatus;

  useEffect(() => {
    let closed = false;
    let handle: GfxHandle | null = null;
    setEnded(undefined);
    setScreen(null);
    statusRef.current("connecting");
    const el = box.current;
    const width = Math.max(640, Math.round(el?.clientWidth ?? 1024));
    const height = Math.max(480, Math.round(el?.clientHeight ?? 768));
    const resize = (w: number, h: number) => {
      size.current = { w, h };
      const c = canvas.current;
      if (c) {
        c.width = w;
        c.height = h;
      }
      setScreen({ w, h });
    };
    const onEvent = (e: GfxEvent) => {
      if (closed) return;
      switch (e.event) {
        case "frame": {
          const ctx = canvas.current?.getContext("2d");
          if (ctx && e.width && e.height) ctx.putImageData(new ImageData(e.rgba, e.width, e.height), e.x, e.y);
          break;
        }
        case "connected":
          setPrompt(null);
          resize(e.width, e.height);
          statusRef.current("connected");
          canvas.current?.focus();
          break;
        case "resized":
          resize(e.width, e.height);
          break;
        case "hostKey":
        case "auth":
          setPrompt(e);
          break;
        case "clipboard":
          sentClip.current = e.text; // don't send it straight back
          void getBackend().then((b) => b.clipboardWrite(e.text));
          break;
        case "audio":
          setHasSound(true);
          player.current.play(e.channels, e.rate, e.pcm);
          break;
        case "cursor": {
          const c = canvas.current;
          if (!c) break;
          if (e.shape === "hidden") c.style.cursor = "none";
          else if (e.shape === "default") c.style.cursor = "";
          else c.style.cursor = cursorCss(e.rgba, e.width, e.height, e.hotX, e.hotY);
          break;
        }
        case "disconnected":
          setPrompt(null);
          // Blank the frozen picture: the message says the session is over.
          canvas.current?.getContext("2d")?.clearRect(0, 0, size.current.w, size.current.h);
          setScreen(null);
          setEnded(e.reason);
          statusRef.current("disconnected");
          break;
      }
    };
    void getBackend()
      .then((b) => b.openGfxSession(conn, { width, height }, onEvent))
      .then((h) => {
        if (closed) h.close();
        else {
          handle = h;
          session.current = h;
        }
      })
      .catch((err) => {
        if (closed) return;
        setEnded(String(err));
        statusRef.current("disconnected");
      });
    return () => {
      closed = true;
      handle?.close();
      session.current = null;
      player.current.close();
      setHasSound(false);
    };
  }, [conn, attempt]);

  useEffect(() => {
    if (!panelId) return;
    reconnect.register(panelId, () => setAttempt((a) => a + 1));
    return () => reconnect.register(panelId, null);
  }, [panelId]);

  // RDP can follow the tab's size (the server decides; VNC ignores it).
  useEffect(() => {
    const el = box.current;
    if (!el || conn.kind !== "rdp") return;
    let timer = 0;
    const ro = new ResizeObserver(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const w = Math.round(el.clientWidth);
        const h = Math.round(el.clientHeight);
        if (w > 200 && h > 200 && (w !== size.current.w || h !== size.current.h)) session.current?.resize(w, h);
      }, 400);
    });
    ro.observe(el);
    return () => {
      window.clearTimeout(timer);
      ro.disconnect();
    };
  }, [conn.kind]);

  /** Canvas coordinates for a pointer event, in remote pixels. */
  const at = (e: { clientX: number; clientY: number }) => {
    const c = canvas.current;
    if (!c) return { x: 0, y: 0 };
    const r = c.getBoundingClientRect();
    const x = Math.round(((e.clientX - r.left) / r.width) * size.current.w);
    const y = Math.round(((e.clientY - r.top) / r.height) * size.current.h);
    return {
      x: Math.max(0, Math.min(size.current.w - 1, x)),
      y: Math.max(0, Math.min(size.current.h - 1, y)),
    };
  };
  const pending = useRef<{ x: number; y: number; b: number } | null>(null);
  const pointer = (e: PointerEvent) => {
    const { x, y } = at(e);
    const b = buttonsOf(e.buttons);
    if (e.type === "pointermove") {
      // One move per frame is plenty.
      const first = pending.current === null;
      pending.current = { x, y, b };
      if (first)
        requestAnimationFrame(() => {
          const p = pending.current;
          pending.current = null;
          if (p) session.current?.pointer(p.x, p.y, p.b);
        });
    } else {
      pending.current = null;
      session.current?.pointer(x, y, b);
    }
    if (e.type === "pointerdown") {
      canvas.current?.focus();
      canvas.current?.setPointerCapture(e.pointerId);
    }
    e.preventDefault();
  };
  const wheel = (e: WheelEvent) => {
    const { x, y } = at(e);
    const notch = (d: number) => (d === 0 ? 0 : Math.sign(d) * Math.max(1, Math.round(Math.abs(d) / 100)));
    session.current?.wheel(x, y, notch(e.deltaX), notch(e.deltaY));
  };
  const key = (e: KeyboardEvent, down: boolean) => {
    // Ctrl+Shift+V pastes the clipboard as typing.
    if (down && e.ctrlKey && e.shiftKey && e.code === "KeyV") {
      e.preventDefault();
      void getBackend()
        .then((b) => b.clipboardRead())
        .then((t) => t && session.current?.text(t));
      return;
    }
    e.preventDefault();
    session.current?.key(e.code, e.key, down);
  };

  /** VNC: when the screen gets focus, offer the local clipboard if it changed,
   *  so a plain Ctrl+V on the remote side pastes it. */
  const shareClipboard = () => {
    if (conn.kind !== "vnc" && conn.kind !== "rdp") return;
    void getBackend()
      .then((b) => b.clipboardRead())
      .then((t) => {
        if (!t || t === sentClip.current) return;
        sentClip.current = t;
        session.current?.clipboard(t);
      });
  };

  const reply = (r: PromptReply) => {
    setPrompt(null);
    session.current?.reply(r);
    canvas.current?.focus();
  };

  return (
    <div className="remote-pane" ref={box}>
      <canvas
        ref={canvas}
        className="remote-screen"
        tabIndex={0}
        style={screen ? { aspectRatio: `${screen.w} / ${screen.h}` } : { display: "none" }}
        onPointerDown={pointer}
        onPointerUp={pointer}
        onPointerMove={pointer}
        onWheel={wheel}
        onFocus={shareClipboard}
        onKeyDown={(e) => key(e, true)}
        onKeyUp={(e) => key(e, false)}
        onContextMenu={(e) => e.preventDefault()}
        aria-label={`${kindLabel[conn.kind]} screen of ${conn.name}`}
      />
      {!screen && ended === undefined && !prompt && (
        <div className="remote-status">
          <KindIcon kind={conn.kind} size={32} />
          <p>Connecting to {conn.endpoint}…</p>
        </div>
      )}
      {ended !== undefined && (
        <div className="remote-status">
          <KindIcon kind={conn.kind} size={32} />
          <p>{ended ? `Disconnected: ${ended}` : "Disconnected"}</p>
          <button className="btn" onClick={() => setAttempt((a) => a + 1)}>
            <RotateCw size={14} /> Reconnect
          </button>
        </div>
      )}
      {hasSound && (
        <button
          className="remote-sound"
          title={muted ? "Unmute sound" : "Mute sound"}
          aria-label={muted ? "Unmute sound" : "Mute sound"}
          onClick={() => {
            player.current.muted = !muted;
            setMuted(!muted);
            canvas.current?.focus();
          }}
        >
          {muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      )}
      {prompt && <SessionPrompt prompt={prompt} onReply={reply} />}
    </div>
  );
}

/** A CSS `cursor` value drawing the server's pointer (RGBA pixels, base64). */
function cursorCss(rgbaBase64: string, width: number, height: number, hotX: number, hotY: number): string {
  const bytes = Uint8ClampedArray.from(atob(rgbaBase64), (ch) => ch.charCodeAt(0));
  if (bytes.length !== width * height * 4) return "";
  const c = document.createElement("canvas");
  c.width = width;
  c.height = height;
  c.getContext("2d")?.putImageData(new ImageData(bytes, width, height), 0, 0);
  return `url(${c.toDataURL()}) ${hotX} ${hotY}, default`;
}
