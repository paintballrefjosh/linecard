import { createContext, useCallback, useContext, useState } from "react";
import {
  DockviewReact,
  type DockviewApi,
  type DockviewGroupPanel,
  type IDockviewPanelHeaderProps,
  type IDockviewPanelProps,
  type IWatermarkPanelProps,
} from "dockview";
import "dockview/dist/styles/dockview.css";
import { X } from "lucide-react";
import type { ConnectionNode, LayoutId } from "../types";
import type { Theme } from "../theme";
import { KindIcon } from "./icons";
import { GraphicalPlaceholder, TerminalView, type SessionStatus } from "./TerminalView";

interface PanelParams {
  conn: ConnectionNode;
}

interface Shared {
  theme: Theme;
  status: Record<string, SessionStatus>;
  setStatus(panelId: string, s: SessionStatus): void;
}

const SharedCtx = createContext<Shared>({ theme: "dark", status: {}, setStatus: () => {} });

function SessionPanel({ api, params }: IDockviewPanelProps<PanelParams>) {
  const { theme, setStatus } = useContext(SharedCtx);
  const { conn } = params;
  if (conn.kind === "rdp" || conn.kind === "vnc") return <GraphicalPlaceholder conn={conn} />;
  return <TerminalView conn={conn} theme={theme} onStatus={(s) => setStatus(api.id, s)} />;
}

function SessionTab({ api, params }: IDockviewPanelHeaderProps<PanelParams>) {
  const { status } = useContext(SharedCtx);
  const conn = params.conn;
  const s = status[api.id] ?? "connecting";
  return (
    <div className="session-tab" style={conn.tabColor ? { boxShadow: `inset 0 -2px 0 ${conn.tabColor}` } : undefined}>
      <span className={`led led-${s}`} title={s} />
      <KindIcon kind={conn.kind} size={14} />
      <span className="session-tab-title">{api.title}</span>
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

/** Open a connection as a tab in the active pane. */
export function openConnection(api: DockviewApi, conn: ConnectionNode) {
  api.addPanel<PanelParams>({
    id: `${conn.id}#${++counter}`,
    component: "session",
    tabComponent: "session",
    title: conn.name,
    params: { conn },
    position: api.activeGroup ? { referenceGroup: api.activeGroup } : undefined,
  });
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
  return (
    <SharedCtx.Provider value={{ theme, status, setStatus }}>
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
