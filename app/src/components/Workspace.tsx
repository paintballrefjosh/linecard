import { createContext, useCallback, useContext, useEffect, useRef, useState, type CSSProperties } from "react";
import {
  DockviewReact,
  type DockviewApi,
  type DockviewGroupPanel,
  type IDockviewPanelHeaderProps,
  type IDockviewPanelProps,
  type IWatermarkPanelProps,
  type SerializedDockview,
} from "dockview-react";
import "dockview-react/dist/styles/dockview.css";
import { CopyPlus, FolderOpen, Radio, RotateCw, X } from "lucide-react";
import type { ConnectionNode, LayoutId } from "../types";
import type { Theme } from "../theme";
import { KindIcon } from "./icons";
import { TerminalView, type SessionStatus } from "./TerminalView";
import { RemoteView } from "./RemoteView";
import { SftpView } from "./SftpView";
import { Modal } from "./Dialog";
import { ContextMenu } from "./ContextMenu";
import { reconnect } from "../reconnect";
import { usePrefs } from "../prefs";
import { broadcast, useBroadcast } from "../broadcast";

interface PanelParams {
  conn: ConnectionNode;
  /** Sessions to the same connection: 1 for the first, 2 for "spine01 (2)"… */
  instance: number;
  /** A file browser (SFTP) on this SSH connection, not a terminal. */
  files?: boolean;
}

interface Shared {
  theme: Theme;
  status: Record<string, SessionStatus>;
  setStatus(panelId: string, s: SessionStatus): void;
  /** Panels that received output while hidden behind another tab. */
  activity: Record<string, boolean>;
  /** Close a tab, asking first if its session is open. */
  askClose(panelId: string): void;
  setActivity(panelId: string, on: boolean): void;
  /** Titles sessions set for themselves (OSC 0/2), by panel. */
  titles: Record<string, string>;
  setTitle(panelId: string, title: string): void;
}

const SharedCtx = createContext<Shared>({
  theme: "dark",
  status: {},
  setStatus: () => {},
  activity: {},
  setActivity: () => {},
  askClose: () => {},
  titles: {},
  setTitle: () => {},
});

function SessionPanel({ api, params }: IDockviewPanelProps<PanelParams>) {
  const { theme, setStatus, setActivity, setTitle } = useContext(SharedCtx);
  const { conn } = params;
  useEffect(() => {
    const sub = api.onDidVisibilityChange((e) => e.isVisible && setActivity(api.id, false));
    return () => sub.dispose();
  }, [api, setActivity]);
  // Whether this is the tab in use, so scripts know which session is "active".
  const [active, setActive] = useState(api.isActive);
  useEffect(() => {
    const sub = api.onDidActiveChange((e) => setActive(e.isActive));
    return () => sub.dispose();
  }, [api]);
  if (params.files) return <SftpView conn={conn} panelId={api.id} onStatus={(s) => setStatus(api.id, s)} />;
  if (conn.kind === "rdp" || conn.kind === "vnc")
    return <RemoteView conn={conn} panelId={api.id} onStatus={(s) => setStatus(api.id, s)} />;
  return (
    <TerminalView
      panelId={api.id}
      active={active}
      conn={conn}
      theme={theme}
      onStatus={(s) => setStatus(api.id, s)}
      onTitle={(t) => setTitle(api.id, t)}
      onOutput={() => !api.isVisible && setActivity(api.id, true)}
    />
  );
}

/** Put the keyboard in a tab's session (terminal or remote screen). Selecting
 *  a tab leaves focus on the tab itself, so this runs just after. */
function focusSession(dock: DockviewApi, id: string) {
  const go = () =>
    dock
      .getPanel(id)
      ?.view.content.element.querySelector<HTMLElement>("textarea.xterm-helper-textarea, canvas")
      ?.focus();
  requestAnimationFrame(go);
  window.setTimeout(go, 60);
}

function SessionTab({ api, containerApi, params }: IDockviewPanelHeaderProps<PanelParams>) {
  const { status, activity, askClose, titles } = useContext(SharedCtx);
  const { prefs } = usePrefs();
  const conn = params.conn;
  // The session's own title (e.g. "user@host: ~/src" or what Claude Code
  // sets), if it set one; the connection's name otherwise.
  const sessionTitle = prefs.tabTitleFromSession ? titles[api.id] : undefined;
  const label = sessionTitle || api.title;
  const s = status[api.id] ?? "connecting";
  const active = activity[api.id];
  const { on: casting, members } = useBroadcast();
  const member = members.has(api.id);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const root = useRef<HTMLDivElement>(null);
  // Dockview drags a tab with a copy of the whole tab (every computed style, icons and buttons)
  // as the drag image; over Remote Desktop the drop areas stopped showing for it, while the
  // plain-text image it uses for the empty header area worked. Replace it with the same kind.
  useEffect(() => {
    const tab = root.current?.closest<HTMLElement>(".dv-tab");
    if (!tab) return;
    const title = api.title ?? "";
    const onDragStart = (e: DragEvent) => {
      if (!e.dataTransfer) return;
      const ghost = document.createElement("div");
      ghost.className = "tab-drag-image";
      ghost.textContent = title;
      document.body.appendChild(ghost);
      e.dataTransfer.setDragImage(ghost, 30, -10);
      window.setTimeout(() => ghost.remove(), 0);
    };
    // Added after dockview's own listener on this element, so this image is the one used.
    tab.addEventListener("dragstart", onDragStart);
    return () => tab.removeEventListener("dragstart", onDragStart);
  }, [api]);
  return (
    <div
      ref={root}
      className={`session-tab ${active ? "has-activity" : ""}`}
      style={conn.tabColor ? ({ "--tab-color": conn.tabColor } as CSSProperties) : undefined}
      title={[
        sessionTitle,
        `${api.title} · ${conn.kind === "local" ? "this computer" : conn.endpoint}`,
        active ? "New output" : "",
      ]
        .filter(Boolean)
        .join("\n")}
      // Clicking a tab leaves focus on the tab; the keyboard belongs to the session.
      onClick={() => focusSession(containerApi, api.id)}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
      // Middle-click closes, as in browsers (mousedown would start autoscroll).
      onMouseDown={(e) => e.button === 1 && e.preventDefault()}
      onAuxClick={(e) => {
        if (e.button !== 1) return;
        e.preventDefault();
        e.stopPropagation();
        askClose(api.id);
      }}
    >
      <span className={`led led-${s}`} aria-label={s} />
      {params.files ? <FolderOpen size={14} style={{ color: "var(--tab-color, var(--kind-ssh))" }} /> : <KindIcon kind={conn.kind} size={14} />}
      <span className="session-tab-title">{label}</span>
      {active && <span className="activity-dot" aria-label="New output" />}
      {casting && !params.files && conn.kind !== "rdp" && conn.kind !== "vnc" && (
        <button
          className={`tab-broadcast ${member ? "is-member" : ""}`}
          aria-pressed={member}
          aria-label={member ? `Stop broadcasting to ${api.title}` : `Include ${api.title} in broadcast`}
          title={member ? "Receiving broadcast input (click to leave)" : "Not in the broadcast (click to join)"}
          onClick={(e) => {
            e.stopPropagation();
            broadcast.toggleMember(api.id);
          }}
        >
          <Radio size={12} />
        </button>
      )}
      <button
        className="tab-close"
        aria-label={`Close ${api.title}`}
        onClick={(e) => {
          e.stopPropagation();
          askClose(api.id);
        }}
      >
        <X size={13} />
      </button>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label={` tab`}
          onClose={() => setMenu(null)}
          items={[
            {
              label: "Reconnect",
              icon: <RotateCw size={15} />,
              disabled: s !== "disconnected",
              onSelect: () => reconnect.run(api.id),
            },
            {
              label: "Clone session",
              icon: <CopyPlus size={15} />,
              onSelect: () => focusSession(containerApi, openConnection(containerApi, conn, { files: params.files, after: api.id })),
            },
            { label: "Close tab", icon: <X size={15} />, onSelect: () => askClose(api.id) },
          ]}
        />
      )}
    </div>
  );
}

function Watermark(_: IWatermarkPanelProps) {
  return (
    <div className="watermark">
      <p>Double-click a connection to open it here, or drag a tab into this pane.</p>
    </div>
  );
}

let counter = 0;

/** Saved connections group by id; quick-connect sessions by host:port. */
const sessionKey = (conn: ConnectionNode) => (conn.id.startsWith("quick-") ? `${conn.kind}:${conn.endpoint}` : conn.id);

/** Open a connection as a tab in the active pane, or just right of the tab
 *  `after` (Clone session). A second session to the same connection is titled
 *  "name (2)"; numbers stay put while a tab is open and the lowest free one is
 *  reused, so the first session is plain "name". Returns the new panel's id. */
export function openConnection(
  api: DockviewApi,
  conn: ConnectionNode,
  opts: { files?: boolean; after?: string } = {},
): string {
  const files = !!opts.files;
  const key = sessionKey(conn);
  const taken = new Set(
    api.panels
      .map((p) => p.params as PanelParams | undefined)
      .filter((p) => p && sessionKey(p.conn) === key && !!p.files === files)
      .map((p) => p!.instance),
  );
  let instance = 1;
  while (taken.has(instance)) instance++;
  const after = opts.after ? api.getPanel(opts.after) : undefined;
  const id = `${conn.id}#${++counter}`;
  api.addPanel<PanelParams>({
    id,
    component: "session",
    tabComponent: "session",
    title: `${conn.name}${files ? " · files" : ""}${instance === 1 ? "" : ` (${instance})`}`,
    params: { conn, instance, ...(files ? { files } : {}) },
    position: after
      ? { referenceGroup: after.group, index: after.group.panels.indexOf(after) + 1 }
      : api.activeGroup
        ? { referenceGroup: api.activeGroup }
        : undefined,
  });
  return id;
}

/** Open tabs and pane layout, for saving as a workspace. */
export function snapshotWorkspace(api: DockviewApi): SerializedDockview {
  return api.toJSON();
}

/** Replace the current tabs with a saved workspace. Connections are refreshed
 *  from `current` by id (so renames and edits since saving apply); ones that
 *  were deleted keep their saved details. */
export function restoreWorkspace(
  api: DockviewApi,
  saved: SerializedDockview,
  current: (id: string) => ConnectionNode | undefined,
) {
  const data: SerializedDockview = structuredClone(saved);
  for (const panel of Object.values(data.panels)) {
    const p = panel.params as PanelParams | undefined;
    if (!p) continue;
    const fresh = current(p.conn.id);
    if (fresh) {
      p.conn = fresh;
      panel.title = p.instance > 1 ? `${fresh.name} (${p.instance})` : fresh.name;
    }
    // Keep new panel ids clear of restored ones.
    const n = Number(panel.id.split("#").pop());
    if (n > counter) counter = n;
  }
  api.clear();
  api.fromJSON(data);
}

/** Rearrange panes into one of the six layouts, keeping every open tab.
 *  Tabs are dealt round-robin across the new panes; empty panes show the
 *  watermark until something is opened or dropped into them. */
export function applyLayout(api: DockviewApi, layout: LayoutId) {
  const first = api.groups[0] ?? api.addGroup();
  // Collapse everything into the first pane.
  for (const panel of [...api.panels]) {
    if (panel.group !== first) panel.api.moveTo({ group: first });
  }
  for (const g of [...api.groups]) if (g !== first) api.removeGroup(g);

  const add = (ref: DockviewGroupPanel, direction: "right" | "below") =>
    api.addGroup({ referenceGroup: ref, direction });

  let groups: DockviewGroupPanel[] = [first];
  switch (layout) {
    case "2v":
      groups = [first, add(first, "right")];
      break;
    case "2h":
      groups = [first, add(first, "below")];
      break;
    case "3c": {
      const b = add(first, "right");
      groups = [first, b, add(b, "right")];
      break;
    }
    case "1+2": {
      const b = add(first, "right");
      groups = [first, b, add(b, "below")];
      break;
    }
    case "4": {
      const b = add(first, "right");
      const c = add(first, "below");
      groups = [first, b, c, add(b, "below")];
      break;
    }
  }
  [...api.panels].forEach((p, i) => {
    const target = groups[i % groups.length];
    if (p.group !== target) p.api.moveTo({ group: target });
  });
}

/** Tabs in screen order: pane by pane, then left to right. */
function tabOrder(api: DockviewApi) {
  return api.groups.flatMap((g) => g.panels);
}

/** Open sessions: tabs whose session isn't disconnected. */
export function openSessionCount(api: DockviewApi, status: Record<string, SessionStatus>): number {
  return api.panels.filter((p) => (status[p.id] ?? "connecting") !== "disconnected").length;
}

export function Workspace({
  theme,
  onReady,
  onOpenCount,
}: {
  theme: Theme;
  onReady(api: DockviewApi): void;
  /** Sessions still open (for the prompt when quitting). */
  onOpenCount?(n: number): void;
}) {
  const { prefs, update } = usePrefs();
  const api = useRef<DockviewApi | null>(null);
  const [closing, setClosing] = useState<string | null>(null);
  const [status, setStatusMap] = useState<Record<string, SessionStatus>>({});
  const setStatus = useCallback(
    (id: string, s: SessionStatus) => setStatusMap((m) => (m[id] === s ? m : { ...m, [id]: s })),
    [],
  );
  const [activity, setActivityMap] = useState<Record<string, boolean>>({});
  // Output arrives in bursts; only re-render when the flag actually flips.
  const setActivity = useCallback(
    (id: string, on: boolean) => setActivityMap((m) => (!!m[id] === on ? m : { ...m, [id]: on })),
    [],
  );
  const [titles, setTitles] = useState<Record<string, string>>({});
  const setTitle = useCallback(
    (id: string, t: string) => setTitles((m) => ((m[id] ?? "") === t ? m : { ...m, [id]: t })),
    [],
  );
  const statusRef = useRef(status);
  statusRef.current = status;
  const askClose = useCallback(
    (id: string) => {
      const panel = api.current?.getPanel(id);
      if (!panel) return;
      const open = (statusRef.current[id] ?? "connecting") !== "disconnected";
      if (open && prefs.confirmCloseTab) setClosing(id);
      else panel.api.close();
    },
    [prefs.confirmCloseTab],
  );

  // Ctrl+Tab / Ctrl+Shift+Tab: next / previous tab, across panes. Caught
  // before terminals and remote screens see the keys.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !e.ctrlKey || e.altKey || e.metaKey) return;
      const a = api.current;
      const tabs = a ? tabOrder(a) : [];
      if (!a || tabs.length < 2) return;
      e.preventDefault();
      e.stopPropagation();
      const at = Math.max(
        0,
        tabs.findIndex((p) => p.id === a.activePanel?.id),
      );
      const next = tabs[(at + (e.shiftKey ? -1 : 1) + tabs.length) % tabs.length];
      next.api.setActive();
      focusSession(a, next.id);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // Forget closed tabs, and report how many sessions are open.
  useEffect(() => {
    const a = api.current;
    if (a) onOpenCount?.(openSessionCount(a, status));
  }, [status, onOpenCount]);

  const closingPanel = closing ? api.current?.getPanel(closing) : undefined;
  return (
    <SharedCtx.Provider value={{ theme, status, setStatus, activity, setActivity, askClose, titles, setTitle }}>
      <DockviewReact
        className={`workspace ${theme === "dark" ? "dockview-theme-dark" : "dockview-theme-light"}`}
        components={{ session: SessionPanel }}
        tabComponents={{ session: SessionTab }}
        watermarkComponent={Watermark}
        onReady={(e) => {
          api.current = e.api;
          e.api.onDidRemovePanel((p) => {
            setStatusMap(({ [p.id]: _, ...rest }) => rest);
            setActivityMap(({ [p.id]: _, ...rest }) => rest);
            broadcast.prune(e.api.panels.map((x) => x.id));
          });
          e.api.onDidAddPanel(() => onOpenCount?.(openSessionCount(e.api, statusRef.current)));
          onReady(e.api);
        }}
      />
      {closingPanel && (
        <CloseConfirm
          title={`Close ${closingPanel.title}?`}
          message="Its session ends."
          confirmLabel="Close tab"
          onConfirm={(dontAsk) => {
            if (dontAsk) update({ confirmCloseTab: false });
            closingPanel.api.close();
            setClosing(null);
          }}
          onCancel={() => setClosing(null)}
        />
      )}
    </SharedCtx.Provider>
  );
}

/** "Close …?" with an optional "Don't ask again". */
export function CloseConfirm({
  title,
  message,
  confirmLabel,
  offerDontAsk = true,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: string;
  confirmLabel: string;
  offerDontAsk?: boolean;
  onConfirm(dontAskAgain: boolean): void;
  onCancel(): void;
}) {
  const [dontAsk, setDontAsk] = useState(false);
  return (
    <Modal
      title={title}
      className="modal-confirm"
      onSubmit={() => onConfirm(dontAsk)}
      onClose={onCancel}
      actions={
        <>
          {offerDontAsk && (
            <label className="modal-foot-note close-dont-ask">
              <input type="checkbox" checked={dontAsk} onChange={(e) => setDontAsk(e.target.checked)} />
              Don't ask again
            </label>
          )}
          <button type="button" className="btn" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="btn btn-danger" data-autofocus>
            {confirmLabel}
          </button>
        </>
      }
    >
      <p>{message}</p>
    </Modal>
  );
}
