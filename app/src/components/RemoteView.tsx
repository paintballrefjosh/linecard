import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from "react";
import { RotateCw } from "lucide-react";
import { getBackend, type GfxHandle } from "../api";
import type { AuthEvent, ConnectionNode, GfxEvent, HostKeyEvent, PromptReply } from "../types";
import { KindIcon, kindLabel } from "./icons";
import { SessionPrompt } from "./SessionPrompt";
import type { SessionStatus } from "./TerminalView";

interface Props {
  conn: ConnectionNode;
  onStatus(status: SessionStatus): void;
}

/** Browser buttons (left 1, right 2, middle 4) → ours (left 1, middle 2, right 4). */
const buttonsOf = (b: number) => (b & 1) | ((b & 4) >> 1) | ((b & 2) << 1);

/** A VNC or RDP session: the remote screen on a canvas, scaled to fit the
 *  tab, with mouse and keyboard going back to it. */
export function RemoteView({ conn, onStatus }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const session = useRef<GfxHandle | null>(null);
  const size = useRef({ w: 0, h: 0 });
  const [screen, setScreen] = useState<{ w: number; h: number } | null>(null);
  const [prompt, setPrompt] = useState<HostKeyEvent | AuthEvent | null>(null);
  const [ended, setEnded] = useState<string | null | undefined>(undefined);
  const [attempt, setAttempt] = useState(0);
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
          void getBackend().then((b) => b.clipboardWrite(e.text));
          break;
        case "disconnected":
          setPrompt(null);
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
    };
  }, [conn, attempt]);

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
      {prompt && <SessionPrompt prompt={prompt} onReply={reply} />}
    </div>
  );
}
