import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { getBackend, type SessionHandle } from "../api";
import { terminalThemes, type Theme } from "../theme";
import type {
  AuthEvent,
  ConnectionNode,
  ForwardState,
  HostKeyEvent,
  PromptReply,
  TransferEvent,
  TransferProtocol,
} from "../types";
import {
  ArrowLeftRight,
  ClipboardPaste,
  Copy,
  Download,
  Eraser,
  RotateCw,
  TextSelect,
  Upload,
  Zap,
} from "lucide-react";
import { attachHighlighter, highlightStore } from "../highlight";
import { pasteLineCount, usePrefs } from "../prefs";
import { ContextMenu } from "./ContextMenu";
import { KindIcon, kindLabel } from "./icons";
import { PasteConfirm, SessionPrompt } from "./SessionPrompt";

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

  const { prefs } = usePrefs();
  const prefsRef = useRef(prefs);
  prefsRef.current = prefs;
  const themeRef = useRef(theme);
  themeRef.current = theme;
  /** This connection's keyword-highlight profile (follows folder and Settings changes). */
  const profileId = useRef<string | null>(null);
  useEffect(() => {
    const load = () =>
      void getBackend()
        .then((b) => b.effectiveHighlight(conn.id))
        .then((id) => (profileId.current = id));
    load();
    return highlightStore.subscribe(load);
  }, [conn.id]);
  /** Multi-line text waiting for the user to confirm the paste. */
  const [pending, setPending] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [transfer, setTransfer] = useState<TransferEvent | null>(null);
  /** This session's port forwards (SSH). */
  const [forwards, setForwards] = useState<ForwardState[]>([]);
  const [showForwards, setShowForwards] = useState(false);

  /** Pick a file and start an XMODEM/YMODEM transfer on this serial line. */
  const startTransfer = async (send: boolean, protocol: TransferProtocol) => {
    const b = await getBackend();
    const path = await b.pickFile(send ? "open" : "save", send ? "File to send" : "Save received file as");
    if (!path || !sessionRef.current) return;
    if (send)
      term.current?.write(
        `\r\n\x1b[2m[Waiting for the device to start receiving ${path.split(/[\\/]/).pop()}…]\x1b[0m\r\n`,
      );
    try {
      await sessionRef.current.startTransfer(send, protocol, path);
    } catch (err) {
      term.current?.write(`\r\n\x1b[31m[${err}]\x1b[0m\r\n`);
    }
  };
  const [status, setStatus] = useState<SessionStatus>("connecting");
  const report = (st: SessionStatus) => {
    setStatus(st);
    statusRef.current(st);
  };
  /** Reconnect if the session has ended (set by the session effect). */
  const reconnectRef = useRef<() => void>(() => {});

  const reply = (r: PromptReply) => {
    setPrompt(null);
    if (sessionRef.current) sessionRef.current.reply(r);
    else pendingReply.current = r;
    term.current?.focus();
  };

  /** Every paste (right-click, keys, menu, Ctrl+V) comes through here. */
  const pasteText = (text: string) => {
    if (!text || !term.current) return;
    if (prefsRef.current.confirmMultilinePaste && pasteLineCount(text) > 1) setPending(text);
    else term.current.paste(text);
  };
  const pasteClipboard = () => void getBackend().then((b) => b.clipboardRead().then(pasteText));
  const copySelection = () => {
    const text = term.current?.getSelection();
    if (text) void getBackend().then((b) => b.clipboardWrite(text));
  };
  const finishPaste = (ok: boolean) => {
    if (ok && pending) term.current?.paste(pending);
    setPending(null);
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
    /** True between a disconnect and the next connect: Enter reconnects. */
    let ended = false;
    /** Each attempt gets a number so late events from an old one are ignored. */
    let attempt = 0;

    const ended_ = (reason: string | null) => {
      ended = true;
      setPrompt(null);
      report("disconnected");
      const why = reason ? `: ${reason}` : "";
      t.write(`\r\n\x1b[2m[Disconnected${why}] Press Enter to reconnect.\x1b[0m\r\n`);
    };

    const start = () => {
      const mine = ++attempt;
      ended = false;
      report("connecting");
      t.write(`\x1b[2mConnecting to ${conn.endpoint}…\x1b[0m\r\n`);
      getBackend()
        .then((b) =>
          b.openSession(conn, { cols: t.cols, rows: t.rows }, (e) => {
            if (mine !== attempt) return;
            if (e.event === "data") {
              t.write(new Uint8Array(e.data));
              outputRef.current?.();
            } else if (e.event === "hostKey" || e.event === "auth") {
              setPrompt(e);
              outputRef.current?.();
            } else if (e.event === "transfer") {
              setTransfer(e.finished ? null : e);
              if (e.finished) {
                const what = `${e.sending ? "Sent" : "Received"} ${e.file} (${formatBytes(e.done)})`;
                t.write(
                  e.error
                    ? `\r\n\x1b[31m[Transfer of ${e.file} failed: ${e.error}]\x1b[0m\r\n`
                    : `\r\n\x1b[32m[${what}]\x1b[0m\r\n`,
                );
              }
            } else if (e.event === "connected") {
              setPrompt(null);
              report("connected");
            } else if (e.event === "forwards") setForwards(e.forwards);
            else if (e.event === "title") document.title = `${e.title} · Linecard`;
            else if (e.event === "disconnected") {
              setForwards([]);
              // Free the backend's handle; this attempt is over.
              session?.close();
              session = null;
              sessionRef.current = null;
              ended_(e.reason);
            }
          }),
        )
        .then((s) => {
          if (disposed || mine !== attempt) return s.close();
          if (ended) return s.close(); // it already dropped
          session = s;
          sessionRef.current = s;
          if (pendingReply.current) {
            s.reply(pendingReply.current);
            pendingReply.current = null;
          }
        })
        .catch((err) => {
          if (mine !== attempt) return;
          t.write(`\x1b[31mCould not connect: ${err}\x1b[0m`);
          ended_(null);
        });
    };
    reconnectRef.current = () => {
      if (ended) start();
    };
    start();

    const highlighter = attachHighlighter(t, () => ({
      enabled: prefsRef.current.highlightKeywords,
      profile: highlightStore.profile(profileId.current),
      theme: themeRef.current,
    }));

    const input = t.onData((d) => {
      if (ended) {
        if (d === "\r") start();
        return;
      }
      session?.write(d);
    });

    // Copy on select: wait for the drag to settle rather than copying every step.
    let copyTimer = 0;
    const selection = t.onSelectionChange(() => {
      window.clearTimeout(copyTimer);
      if (!prefsRef.current.copyOnSelect || !t.hasSelection()) return;
      copyTimer = window.setTimeout(copySelection, 150);
    });
    t.attachCustomKeyEventHandler((e) => {
      const copyKey = (e.ctrlKey && e.shiftKey && e.code === "KeyC") || (e.ctrlKey && e.key === "Insert");
      const pasteKey = (e.ctrlKey && e.shiftKey && e.code === "KeyV") || (e.shiftKey && e.key === "Insert");
      const breakKey = conn.kind === "serial" && e.ctrlKey && e.shiftKey && e.code === "KeyB";
      if (!copyKey && !pasteKey && !breakKey) return true;
      if (e.type === "keydown") {
        e.preventDefault();
        if (copyKey) copySelection();
        else if (breakKey) session?.sendBreak();
        else pasteClipboard();
      }
      return false;
    });
    // Ctrl+V and other native pastes: route through the same confirm step.
    const el = host.current;
    const onPaste = (e: ClipboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      pasteText(e.clipboardData?.getData("text/plain") ?? "");
    };
    const onContextMenu = (e: MouseEvent) => {
      e.preventDefault();
      if (prefsRef.current.rightClick === "paste") pasteClipboard();
      else setMenu({ x: e.clientX, y: e.clientY });
    };
    el.addEventListener("paste", onPaste, true);
    el.addEventListener("contextmenu", onContextMenu);
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
      window.clearTimeout(copyTimer);
      ro.disconnect();
      el.removeEventListener("paste", onPaste, true);
      el.removeEventListener("contextmenu", onContextMenu);
      selection.dispose();
      highlighter.dispose();
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
      {forwards.length > 0 && (
        <div className="forward-chip-wrap">
          <button
            className={`forward-chip ${forwards.some((f) => f.error) ? "has-error" : ""}`}
            aria-expanded={showForwards}
            onClick={() => setShowForwards((v) => !v)}
            title="Port forwards"
          >
            <ArrowLeftRight size={12} /> {forwards.filter((f) => !f.error).length}/{forwards.length}
          </button>
          {showForwards && (
            <ul className="forward-list" role="status">
              {forwards.map((f, i) => (
                <li key={i} className={f.error ? "has-error" : ""}>
                  <code>{f.label}</code>
                  {f.error && <span>{f.error}</span>}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {transfer && (
        <div className="transfer-bar" role="status" aria-live="polite">
          <span className="transfer-label">
            {transfer.sending ? "Sending" : "Receiving"} <b>{transfer.file}</b>
          </span>
          <span className="transfer-track">
            <span
              className="transfer-fill"
              style={{ width: transfer.total ? `${Math.min(100, (transfer.done / transfer.total) * 100)}%` : "100%" }}
              data-indeterminate={transfer.total ? undefined : ""}
            />
          </span>
          <span className="transfer-count">
            {formatBytes(transfer.done)}
            {transfer.total ? ` of ${formatBytes(transfer.total)}` : ""}
          </span>
          <button className="btn" onClick={() => sessionRef.current?.cancelTransfer()}>
            Cancel
          </button>
        </div>
      )}
      {pending !== null && <PasteConfirm text={pending} onDone={finishPaste} />}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label="Terminal"
          onClose={() => {
            setMenu(null);
            term.current?.focus();
          }}
          items={[
            {
              label: "Copy",
              icon: <Copy size={15} />,
              shortcut: "Ctrl+Shift+C",
              disabled: !term.current?.hasSelection(),
              onSelect: copySelection,
            },
            { label: "Paste", icon: <ClipboardPaste size={15} />, shortcut: "Ctrl+Shift+V", onSelect: pasteClipboard },
            "separator",
            ...(conn.kind === "serial"
              ? [
                  {
                    label: "Send file (XMODEM-1K)…",
                    icon: <Upload size={15} />,
                    disabled: status !== "connected" || !!transfer,
                    onSelect: () => void startTransfer(true, "xmodem1k"),
                  },
                  {
                    label: "Send file (XMODEM)…",
                    icon: <Upload size={15} />,
                    disabled: status !== "connected" || !!transfer,
                    onSelect: () => void startTransfer(true, "xmodem"),
                  },
                  {
                    label: "Send file (YMODEM)…",
                    icon: <Upload size={15} />,
                    disabled: status !== "connected" || !!transfer,
                    onSelect: () => void startTransfer(true, "ymodem"),
                  },
                  {
                    label: "Receive file (XMODEM)…",
                    icon: <Download size={15} />,
                    disabled: status !== "connected" || !!transfer,
                    onSelect: () => void startTransfer(false, "xmodem"),
                  },
                  "separator" as const,
                  {
                    label: "Send Break",
                    icon: <Zap size={15} />,
                    shortcut: "Ctrl+Shift+B",
                    disabled: status !== "connected",
                    onSelect: () => sessionRef.current?.sendBreak(),
                  },
                ]
              : []),
            {
              label: "Reconnect",
              icon: <RotateCw size={15} />,
              disabled: status !== "disconnected",
              onSelect: () => reconnectRef.current(),
            },
            { label: "Select all", icon: <TextSelect size={15} />, onSelect: () => term.current?.selectAll() },
            { label: "Clear scrollback", icon: <Eraser size={15} />, onSelect: () => term.current?.clear() },
          ]}
        />
      )}
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

/** "1.2 MB" */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
