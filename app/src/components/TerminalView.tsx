import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { getBackend, type SessionHandle } from "../api";
import { terminalThemes, type Theme } from "../theme";
import type { AuthEvent, ConnectionNode, HostKeyEvent, PromptReply } from "../types";
import { KindIcon, kindLabel } from "./icons";
import { SessionPrompt } from "./SessionPrompt";

export type SessionStatus = "connecting" | "connected" | "disconnected";

interface Props {
  conn: ConnectionNode;
  theme: Theme;
  onStatus(status: SessionStatus): void;
  /** Called whenever the remote side sends output. */
  onOutput?(): void;
}

const FONT = '"Cascadia Mono", "Cascadia Code", Consolas, "SF Mono", Menlo, "DejaVu Sans Mono", monospace';

export function TerminalView({ conn, theme, onStatus, onOutput }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const statusRef = useRef(onStatus);
  statusRef.current = onStatus;
  const outputRef = useRef(onOutput);
  outputRef.current = onOutput;
  const [prompt, setPrompt] = useState<HostKeyEvent | AuthEvent | null>(null);
  const sessionRef = useRef<SessionHandle | null>(null);
  /** An answer given before openSession resolved; sent once it does. */
  const pendingReply = useRef<PromptReply | null>(null);

  const reply = (r: PromptReply) => {
    setPrompt(null);
    if (sessionRef.current) sessionRef.current.reply(r);
    else pendingReply.current = r;
    term.current?.focus();
  };

  useEffect(() => {
    if (!host.current) return;
    const t = new Terminal({
      fontFamily: FONT,
      fontSize: 13,
      cursorBlink: true,
      scrollback: 10_000,
      allowProposedApi: true,
      theme: terminalThemes[theme],
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(host.current);
    try {
      t.loadAddon(new WebglAddon());
    } catch {
      // WebGL unavailable (VMs, remote desktop): xterm falls back to the DOM renderer.
    }
    fit.fit();
    term.current = t;

    let session: SessionHandle | null = null;
    let disposed = false;
    statusRef.current("connecting");
    t.write(`\x1b[2mConnecting to ${conn.endpoint}…\x1b[0m\r\n`);

    getBackend()
      .then((b) =>
        b.openSession(conn, { cols: t.cols, rows: t.rows }, (e) => {
          if (e.event === "data") {
            t.write(new Uint8Array(e.data));
            outputRef.current?.();
          } else if (e.event === "hostKey" || e.event === "auth") {
            setPrompt(e);
            outputRef.current?.();
          } else if (e.event === "connected") {
            setPrompt(null);
            statusRef.current("connected");
          } else if (e.event === "title") document.title = `${e.title} · Linecard`;
          else if (e.event === "disconnected") {
            setPrompt(null);
            statusRef.current("disconnected");
            t.write(`\r\n\x1b[2m[Disconnected${e.reason ? `: ${e.reason}` : ""}]\x1b[0m\r\n`);
          }
        }),
      )
      .then((s) => {
        if (disposed) return s.close();
        session = s;
        sessionRef.current = s;
        if (pendingReply.current) {
          s.reply(pendingReply.current);
          pendingReply.current = null;
        }
      })
      .catch((err) => {
        statusRef.current("disconnected");
        t.write(`\x1b[31mCould not connect: ${err}\x1b[0m\r\n`);
      });

    const input = t.onData((d) => session?.write(d));
    const resize = t.onResize(({ cols, rows }) => session?.resize(cols, rows));
    // Refit once per frame, never synchronously inside the observer callback.
    let frame = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        try {
          fit.fit();
        } catch {
          /* element hidden (inactive tab) */
        }
      });
    });
    ro.observe(host.current);

    return () => {
      disposed = true;
      cancelAnimationFrame(frame);
      ro.disconnect();
      input.dispose();
      resize.dispose();
      session?.close();
      t.dispose();
      term.current = null;
    };
  }, [conn.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (term.current) term.current.options.theme = terminalThemes[theme];
  }, [theme]);

  return (
    <div className="terminal-pane">
      <div className="terminal-host" ref={host} />
      {prompt && <SessionPrompt prompt={prompt} onReply={reply} />}
    </div>
  );
}

/** Placeholder for graphical sessions until the RDP/VNC drivers land (Phase 3). */
export function GraphicalPlaceholder({ conn }: { conn: ConnectionNode }) {
  return (
    <div className="graphical-placeholder">
      <KindIcon kind={conn.kind} size={40} />
      <p>
        {kindLabel[conn.kind]} sessions arrive in Phase 3. This tab will show {conn.endpoint} here.
      </p>
    </div>
  );
}
