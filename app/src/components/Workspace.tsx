import { createContext, useCallback, useContext, useEffect, useState, type CSSProperties } from "react";
import {
  DockviewReact,
  type DockviewApi,
  type DockviewGroupPanel,
  type IDockviewPanelHeaderProps,
  type IDockviewPanelProps,
  type IWatermarkPanelProps,
  type SerializedDockview,
} from "dockview";
import "dockview/dist/styles/dockview.css";
import { X } from "lucide-react";
import type { ConnectionNode, LayoutId } from "../types";
import type { Theme } from "../theme";
import { KindIcon } from "./icons";
import { TerminalView, type SessionStatus } from "./TerminalView";
import { RemoteView } from "./RemoteView";

interface PanelParams {
  conn: ConnectionNode;
  /** Sessions to the same connection: 1 for the first, 2 for "spine01 (2)"… */
  instance: number;
}

interface Shared {
  theme: Theme;
  status: Record<string, SessionStatus>;
  setStatus(panelId: string, s: SessionStatus): void;
  /** Panels that received output while hidden behind another tab. */
  activity: Record<string, boolean>;
  setActivity(panelId: string, on: boolean): void;
}

const SharedCtx = createContext<Shared>({
  theme: "dark",
  status: {},
  setStatus: () => {},
  activity: {},
  setActivity: () => {},
});

function SessionPanel({ api, params }: IDockviewPanelProps<PanelParams>) {
  const { theme, setStatus, setActivity } = useContext(SharedCtx);
  const { conn } = params;
  useEffect(() => {
    const sub = api.onDidVisibilityChange((e) => e.isVisible && setActivity(api.id, false));
    return () => sub.dispose();
  }, [api, setActivity]);
  if (conn.kind === "rdp" || conn.kind === "vnc")
    return <RemoteView conn={conn} onStatus={(s) => setStatus(api.id, s)} />;
  return (
    <TerminalView
      conn={conn}
      theme={theme}
      onStatus={(s) => setStatus(api.id, s)}
      onOutput={() => !api.isVisible && setActivity(api.id, true)}
    />
  );
}

function SessionTab({ api, params }: IDockviewPanelHeaderProps<PanelParams>) {
  const { status, activity } = useContext(SharedCtx);
  const conn = params.conn;
  const s = status[api.id] ?? "connecting";
  const active = activity[api.id];
  return (
    <div
      className={`session-tab ${active ? "has-activity" : ""}`}
      style={conn.tabColor ? ({ "--tab-color": conn.tabColor } as CSSProperties) : undefined}
      title={`${api.title} · ${conn.endpoint}${active ? " · new output" : ""}`}
    >
      <span className={`led led-${s}`} aria-label={s} />
      <KindIcon kind={conn.kind} size={14} />
      <span className="session-tab-title">{api.title}</span>
      {active && <span className="activity-dot" aria-label="New output" />}
      <button
        className="tab-close"
        aria-label={`Close ${api.title}`}
        onClick={(e) => {
          e.stopPropagation();
          api.close();
        }}
      >
        <X size={13} />
      </button>
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

/** Open a connection as a tab in the active pane. A second session to the
 *  same connection is titled "name (2)"; numbers stay put while a tab is open
 *  and the lowest free one is reused, so the first session is plain "name". */
export function openConnection(api: DockviewApi, conn: ConnectionNode) {
  const key = sessionKey(conn);
  const taken = new Set(
    api.panels
      .map((p) => p.params as PanelParams | undefined)
      .filter((p) => p && sessionKey(p.conn) === key)
      .map((p) => p!.instance),
  );
  let instance = 1;
  while (taken.has(instance)) instance++;
  api.addPanel<PanelParams>({
    id: `${conn.id}#${++counter}`,
    component: "session",
    tabComponent: "session",
    title: instance === 1 ? conn.name : `${conn.name} (${instance})`,
    params: { conn, instance },
    position: api.activeGroup ? { referenceGroup: api.activeGroup } : undefined,
  });
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

export function Workspace({ theme, onReady }: { theme: Theme; onReady(api: DockviewApi): void }) {
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
  return (
    <SharedCtx.Provider value={{ theme, status, setStatus, activity, setActivity }}>
      <DockviewReact
        className={`workspace ${theme === "dark" ? "dockview-theme-dark" : "dockview-theme-light"}`}
        components={{ session: SessionPanel }}
        tabComponents={{ session: SessionTab }}
        watermarkComponent={Watermark}
        onReady={(e) => onReady(e.api)}
      />
    </SharedCtx.Provider>
  );
}
