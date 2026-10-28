import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type FormEvent } from "react";
import type { DockviewApi } from "dockview";
import { Download, Laptop, ListTree, Monitor, Moon, Sun } from "lucide-react";
import { getBackend, type Backend } from "./api";
import { useTheme, type ThemePref } from "./theme";
import { useUpdater } from "./updater";
import type { ConnectionDraft, ConnectionKind, ConnectionNode, Device, LayoutId, SyncState, TreeNode } from "./types";
import { ConnectionEditor, blankDraft } from "./components/ConnectionEditor";
import { ConnectionTree, relativeTime, type TreeActions } from "./components/ConnectionTree";
import { DevicesPage, versionMismatch } from "./components/DevicesPage";
import { LayoutPicker, layoutShortcut } from "./components/LayoutPicker";
import { SidebarResizer, useSidebarWidth } from "./components/SidebarResizer";
import { Workspace, applyLayout, openConnection } from "./components/Workspace";

type View = "connections" | "devices";

/** "ssh admin@10.0.0.1:2222", "telnet cs1 2005", "COM3 9600" → ad-hoc connection. */
export function parseQuickConnect(input: string): ConnectionNode | null {
  const s = input.trim();
  const serial = /^(COM\d+|\/dev\/\S+)(?:\s+(\d+))?$/i.exec(s);
  if (serial) {
    return {
      type: "connection",
      id: `quick-${s}`,
      name: serial[1],
      kind: "serial",
      endpoint: `${serial[1]} ${serial[2] ?? 9600}`,
    };
  }
  const m = /^(ssh|telnet|rdp|vnc)?\s*(?:([^@\s]+)@)?([^\s:]+)(?::(\d+)|\s+(\d+))?$/i.exec(s);
  if (!m) return null;
  const kind = (m[1]?.toLowerCase() ?? "ssh") as ConnectionKind;
  const ports: Record<ConnectionKind, number> = { ssh: 22, telnet: 23, rdp: 3389, vnc: 5900, serial: 0 };
  const port = m[4] ?? m[5] ?? String(ports[kind]);
  return { type: "connection", id: `quick-${s}`, name: m[3], kind, endpoint: `${m[3]}:${port}` };
}

/** "/DC1 Fort Worth/Spine" for a folder id; "/" for the top level. */
function folderPath(nodes: TreeNode[], id: string | null, prefix = ""): string {
  if (!id) return "/";
  for (const n of nodes) {
    if (n.type !== "folder") continue;
    const here = `${prefix}/${n.name}`;
    if (n.id === id) return here;
    const below = folderPath(n.children, id, here);
    if (below !== "/") return below;
  }
  return "/";
}

const themeCycle: Record<ThemePref, ThemePref> = { system: "light", light: "dark", dark: "system" };

export default function App() {
  const { pref, setPref, theme } = useTheme();
  const update = useUpdater();
  const [backend, setBackend] = useState<Backend | null>(null);
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  const [sync, setSync] = useState<SyncState>({ state: "off" });
  const [version, setVersion] = useState("");
  const [view, setView] = useState<View>("connections");
  const [layout, setLayout] = useState<LayoutId>("1");
  const [quick, setQuick] = useState("");
  const [quickError, setQuickError] = useState(false);
  const dock = useRef<DockviewApi | null>(null);
  const [sidebarWidth, setSidebarWidth] = useSidebarWidth();
  const [editing, setEditing] = useState<ConnectionDraft | null>(null);

  const treeActions = useMemo<TreeActions | null>(
    () =>
      backend && {
        newConnection: (folderId) => setEditing(blankDraft(folderId)),
        editConnection: (id) => void backend.getConnection(id).then(setEditing),
        createFolder: (parentId, name) => backend.createFolder(parentId, name),
        rename: (id, name) => backend.renameNode(id, name),
        remove: (id) => backend.deleteNode(id),
        duplicate: (id) => backend.duplicateConnection(id),
      },
    [backend],
  );

  const refresh = useCallback(async (b: Backend) => {
    const [t, d, s] = await Promise.all([b.getTree(), b.getDevices(), b.getSyncState()]);
    setTree(t);
    setDevices(d);
    setSync(s);
  }, []);

  useEffect(() => {
    let unsub = () => {};
    getBackend().then((b) => {
      setBackend(b);
      b.appVersion().then(setVersion);
      refresh(b);
      unsub = b.onTreeChanged(() => refresh(b));
    });
    return () => unsub();
  }, [refresh]);

  const changeLayout = useCallback((id: LayoutId) => {
    setLayout(id);
    if (dock.current) applyLayout(dock.current, id);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.altKey) {
        const id = layoutShortcut(e.key);
        if (id) {
          e.preventDefault();
          changeLayout(id);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [changeLayout]);

  const open = (conn: ConnectionNode) => {
    setView("connections");
    if (dock.current) openConnection(dock.current, conn);
  };

  const submitQuick = (e: FormEvent) => {
    e.preventDefault();
    const conn = parseQuickConnect(quick);
    setQuickError(!conn);
    if (conn) {
      open(conn);
      setQuick("");
    }
  };

  const mismatch = versionMismatch(devices);
  const ThemeIcon = pref === "system" ? Monitor : pref === "light" ? Sun : Moon;

  return (
    <div className="app" style={{ "--sidebar-w": `${sidebarWidth}px` } as CSSProperties}>
      <nav className="rail" aria-label="Views">
        <button
          className={view === "connections" ? "is-active" : ""}
          onClick={() => setView("connections")}
          title="Connections"
          aria-label="Connections"
        >
          <ListTree size={20} />
        </button>
        <button
          className={view === "devices" ? "is-active" : ""}
          onClick={() => setView("devices")}
          title="Devices"
          aria-label="Devices"
        >
          <Laptop size={20} />
          {mismatch && <span className="rail-dot" aria-label="Version mismatch" />}
        </button>
        <div className="rail-spacer" />
        <button
          onClick={() => setPref(themeCycle[pref])}
          title={`Theme: ${pref}`}
          aria-label={`Theme: ${pref}. Change theme`}
        >
          <ThemeIcon size={18} />
        </button>
      </nav>

      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <i />
            <i />
            <i />
            <i />
          </span>
          Linecard
        </div>
        {backend && treeActions && (
          <ConnectionTree
            tree={tree}
            actions={treeActions}
            onOpen={open}
            onAck={(id) => backend.ackChange(id)}
            onAckAll={() => backend.ackAll()}
            onRevert={(op) => backend.revert(op)}
            onMove={(id, parent, index) => backend.moveNode(id, parent, index)}
          />
        )}
        <SidebarResizer width={sidebarWidth} onChange={setSidebarWidth} />
      </aside>

      {editing && backend && (
        <ConnectionEditor
          initial={editing}
          location={folderPath(tree, editing.folderId)}
          onSave={async (d) => void (await backend.saveConnection(d))}
          onClose={() => setEditing(null)}
        />
      )}

      <main className="main">
        <header className="toolbar">
          <form className={`quick-connect ${quickError ? "has-error" : ""}`} onSubmit={submitQuick}>
            <input
              value={quick}
              onChange={(e) => {
                setQuick(e.target.value);
                setQuickError(false);
              }}
              placeholder="Quick connect: ssh admin@10.0.0.1, telnet cs1 2005, COM3 9600"
              aria-label="Quick connect"
              aria-invalid={quickError}
            />
          </form>
          <LayoutPicker value={layout} onChange={changeLayout} />
        </header>

        <div className="content">
          <div className="workspace-wrap" hidden={view !== "connections"}>
            <Workspace
              theme={theme}
              onReady={(api) => {
                dock.current = api;
              }}
            />
          </div>
          {view === "devices" && <DevicesPage devices={devices} />}
        </div>

        <footer className="statusbar">
          <SyncStatus sync={sync} />
          {mismatch && (
            <button className="status-link warn" onClick={() => setView("devices")}>
              Devices on different versions
            </button>
          )}
          <span className="status-spacer" />
          {update && (
            <button className="status-link accent" onClick={() => update.installAndRestart()} title={update.notes}>
              <Download size={13} /> Update {update.version} ready — restart to install
            </button>
          )}
          <span className="status-version">v{version}</span>
        </footer>
      </main>
    </div>
  );
}

function SyncStatus({ sync }: { sync: SyncState }) {
  switch (sync.state) {
    case "off":
      return <span className="sync sync-off">Sync off</span>;
    case "synced":
      return (
        <span className="sync sync-ok">
          Synced via {sync.transport} · {relativeTime(sync.at)}
        </span>
      );
    case "syncing":
      return <span className="sync sync-busy">Syncing via {sync.transport}</span>;
    case "offline":
      return <span className="sync sync-warn">Offline · {sync.pending} changes waiting</span>;
    case "error":
      return <span className="sync sync-err">Sync error: {sync.message}</span>;
  }
}
