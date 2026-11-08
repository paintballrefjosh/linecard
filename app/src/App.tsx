import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { DockviewApi } from "dockview-react";
import {
  Download,
  FileCode2,
  Laptop,
  ListTree,
  Monitor,
  Moon,
  Search,
  Settings as SettingsIcon,
  SquareTerminal,
  Sun,
} from "lucide-react";
import { getBackend, isTauri, type Backend } from "./api";
import { useTheme, type ThemePref } from "./theme";
import { useUpdater } from "./updater";
import { PrefsContext, defaultPrefs, withDefaults, type Preferences } from "./prefs";
import type {
  ConnectionDraft,
  ConnectionKind,
  ConnectionNode,
  Device,
  Launch,
  LayoutId,
  LocalShell,
  SyncState,
  TreeNode,
} from "./types";
import { BulkEditor } from "./components/BulkEditor";
import { ConnectionEditor, blankDraft } from "./components/ConnectionEditor";
import { FolderSettingsDialog } from "./components/FolderSettings";
import { UpdateDialog } from "./components/UpdateDialog";
import { highlightStore } from "./highlight";
import { ImportDialog } from "./components/ImportDialog";
import { PasswordDialog } from "./components/PasswordDialog";
import { ConnectionTree, relativeTime, type TreeActions } from "./components/ConnectionTree";
import { DevicesPage, versionMismatch } from "./components/DevicesPage";
import { BroadcastButton } from "./components/BroadcastButton";
import { ButtonBar } from "./components/ButtonBar";
import { ScriptsDialog } from "./components/ScriptsDialog";
import { isDue } from "./schedule";
import { scripts, useScripts } from "./scripts";
import { screens } from "./screens";
import { LayoutPicker, layoutShortcut } from "./components/LayoutPicker";
import { ZoomControl } from "./components/ZoomControl";
import { SettingsPage } from "./components/SettingsPage";
import { ConnectionPanel, usePanelLayout } from "./components/ConnectionPanel";
import { CloseConfirm, Workspace, applyLayout, openConnection } from "./components/Workspace";
import { WorkspaceMenu } from "./components/WorkspaceMenu";
import { CommandPalette } from "./components/CommandPalette";
import { localConnection, remember, shellItems } from "./palette";
import { ShellMenu } from "./components/ShellMenu";

type View = "connections" | "devices" | "settings";

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
  const ports: Record<ConnectionKind, number> = { ssh: 22, telnet: 23, rdp: 3389, vnc: 5900, serial: 0, local: 0 };
  const port = m[4] ?? m[5] ?? String(ports[kind]);
  return { type: "connection", id: `quick-${s}`, name: m[3], kind, endpoint: `${m[3]}:${port}` };
}

function findConnection(nodes: TreeNode[], id: string): ConnectionNode | undefined {
  for (const n of nodes) {
    if (n.type === "connection" && n.id === id) return n;
    if (n.type === "folder") {
      const hit = findConnection(n.children, id);
      if (hit) return hit;
    }
  }
  return undefined;
}

/** Every saved SSH connection (candidates for a jump host). */
function sshConnections(nodes: TreeNode[]): ConnectionNode[] {
  return nodes.flatMap((n) => (n.type === "folder" ? sshConnections(n.children) : n.kind === "ssh" ? [n] : []));
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

const REPO = "https://github.com/paintballrefjosh/linecard";

/** A release's page lists its changes; development builds get the changelog. */
function changelogUrl(version: string): string {
  return /^\d+\.\d+\.\d+$/.test(version) ? `${REPO}/releases/tag/v${version}` : `${REPO}/blob/master/CHANGELOG.md`;
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
  const [scriptsOpen, setScriptsOpen] = useState(false);
  const script = useScripts();
  // A script that asks a question needs its answer: bring the window up.
  useEffect(() => {
    if (script.ask) setScriptsOpen(true);
  }, [script.ask]);
  const dock = useRef<DockviewApi | null>(null);
  const [dockApi, setDockApi] = useState<DockviewApi | null>(null);
  const panel = usePanelLayout();
  /** Auto-hidden connections pane: showing right now. */
  const [peek, setPeek] = useState(false);
  const [editing, setEditing] = useState<ConnectionDraft | null>(null);
  /** Several connections being edited together. */
  const [bulk, setBulk] = useState<ConnectionDraft[] | null>(null);
  const saved = useRef<string | null>(null);
  /** Asks the tree to select and focus a connection (after saving it). */
  const [focusRequest, setFocusRequest] = useState<{ id: string; ids?: string[]; n: number } | null>(null);
  const [passwordFor, setPasswordFor] = useState<{ id: string; name: string } | null>(null);
  const [prefs, setPrefs] = useState<Preferences>(defaultPrefs);
  // Scheduled scripts (Scripts window → Run on a schedule), checked every few seconds.
  const schedules = useRef(prefs.schedules);
  schedules.current = prefs.schedules;
  useEffect(() => {
    const seen = new Map<string, number>(); // when each rule was first noticed
    const lastRun = new Map<string, number>();
    const tick = () => {
      const now = new Date();
      for (const rule of schedules.current) {
        if (!seen.has(rule.id)) seen.set(rule.id, now.getTime());
        if (!isDue(rule, now, lastRun.get(rule.id) ?? null, seen.get(rule.id) ?? now.getTime())) continue;
        if (scripts.isRunning(rule.script) || !scripts.canStart()) continue;
        lastRun.set(rule.id, now.getTime());
        void scripts.start(rule.script, { scheduled: true });
      }
    };
    tick();
    const timer = window.setInterval(tick, 15_000);
    return () => window.clearInterval(timer);
  }, []);
  const [importing, setImporting] = useState(false);
  const [showUpdate, setShowUpdate] = useState(false);
  /** Sessions open right now, and whether we're asking before quitting. */
  const [openCount, setOpenCount] = useState(0);
  const [quitting, setQuitting] = useState(false);
  const quitGuard = useRef({ openCount, ask: true });
  quitGuard.current = { openCount, ask: prefs.confirmQuit };
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) =>
      getCurrentWindow()
        .onCloseRequested((e) => {
          // One question for all of them, not one per tab.
          if (quitGuard.current.ask && quitGuard.current.openCount > 0) {
            e.preventDefault();
            setQuitting(true);
          }
        })
        .then((u) => (gone ? u() : (unlisten = u))),
    );
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);
  const quit = async (dontAsk: boolean) => {
    if (dontAsk) prefsValue.update({ confirmQuit: false });
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().destroy();
  };
  /** The tree waits for preferences so folders open as they were left. */
  const [prefsReady, setPrefsReady] = useState(false);
  const [folderSettings, setFolderSettings] = useState<{ id: string; parentId: string | null } | null>(null);

  /** Newest first, kept per computer for the command palette. */
  const rememberOpened = (id: string) =>
    setPrefs((cur) => {
      const next = { ...cur, recent: remember(cur.recent, id) };
      void backend?.setPreferences(next);
      return next;
    });
  const [paletteOpen, setPaletteOpen] = useState(false);
  /** This computer's shells; null until found (WSL can take a moment). */
  const [shells, setShells] = useState<LocalShell[] | null>(null);
  useEffect(() => {
    if (!backend) return;
    void backend
      .localShells()
      .then(setShells)
      .catch(() => setShells([]));
  }, [backend]);
  const shellPalette = useMemo(() => shellItems(shells ?? []), [shells]);
  const openDefaultShell = useRef<() => void>(() => {});

  const prefsValue = useMemo(
    () => ({
      prefs,
      update(patch: Partial<Preferences>) {
        setPrefs((cur) => {
          const next = { ...cur, ...patch };
          void backend?.setPreferences(next);
          return next;
        });
      },
    }),
    [prefs, backend],
  );

  const treeActions = useMemo<TreeActions | null>(
    () =>
      backend && {
        newConnection: (folderId) => setEditing(blankDraft(folderId)),
        editConnection: (ids) => {
          if (ids.length === 1) void backend.getConnection(ids[0]).then(setEditing);
          else if (ids.length > 1) void Promise.all(ids.map((id) => backend.getConnection(id))).then(setBulk);
        },
        setPassword: (id, name) => setPasswordFor({ id, name }),
        openFiles: (id) =>
          void backend.getTree().then((t) => {
            const conn = findConnection(t, id);
            if (conn) openFilesRef.current(conn);
          }),
        folderSettings: (id, parentId) => setFolderSettings({ id, parentId }),
        createFolder: (parentId, name) => backend.createFolder(parentId, name),
        rename: (id, name) => backend.renameNode(id, name),
        remove: (id) => backend.deleteNode(id),
        duplicate: (id) => backend.duplicateConnection(id),
        importConnections: () => setImporting(true),
      },
    [backend],
  );

  /** Sync status and devices: cheap, and changes twice every sync round. */
  const refreshSync = useCallback(async (b: Backend) => {
    const [d, s] = await Promise.all([b.getDevices(), b.getSyncState()]);
    setDevices(d);
    setSync(s);
  }, []);

  /** Everything. Edits and syncs can fire tree-changed in quick bursts: one
   *  refresh runs at a time, and those asked for meanwhile become one more. */
  const refreshing = useRef<{ running: boolean; again: boolean }>({ running: false, again: false });
  const refresh = useCallback(
    async (b: Backend) => {
      const r = refreshing.current;
      if (r.running) {
        r.again = true;
        return;
      }
      r.running = true;
      try {
        do {
          r.again = false;
          const [t, h] = await Promise.all([b.getTree(), b.getHighlight(), refreshSync(b)]);
          highlightStore.set(h);
          setTree(t);
        } while (r.again);
      } finally {
        r.running = false;
      }
    },
    [refreshSync],
  );

  useEffect(() => {
    let unsub = () => {};
    getBackend().then((b) => {
      setBackend(b);
      b.appVersion().then(setVersion);
      b.getPreferences().then((p) => {
        setPrefs(withDefaults(p));
        setPrefsReady(true);
      });
      refresh(b);
      const offTree = b.onTreeChanged(() => refresh(b));
      // A sync that changed the tree also fires tree-changed.
      const offSync = b.onSyncChanged(() => void refreshSync(b));
      unsub = () => {
        offTree();
        offSync();
      };
    });
    return () => unsub();
  }, [refresh, refreshSync]);

  const changeLayout = useCallback((id: LayoutId) => {
    setLayout(id);
    if (dock.current) applyLayout(dock.current, id);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // A reload would drop every open session; keys are still passed to the terminal.
      if (!import.meta.env.DEV && (e.key === "F5" || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r")))
        e.preventDefault();
      // The command palette (Ctrl+K would take a shell's kill-line key).
      if (e.ctrlKey && e.shiftKey && !e.altKey && e.code === "KeyP") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
        return;
      }
      // A new tab with this computer's default shell.
      if (e.ctrlKey && e.shiftKey && !e.altKey && e.code === "KeyT") {
        e.preventDefault();
        openDefaultShell.current();
        return;
      }
      if (e.ctrlKey && e.altKey) {
        const id = layoutShortcut(e.key);
        if (id) {
          e.preventDefault();
          changeLayout(id);
        }
      }
    };
    // The browser's own menu offers Reload; keep it only for text fields (copy and paste).
    const onMenu = (e: MouseEvent) => {
      const t = e.target instanceof Element ? e.target : null;
      if (!import.meta.env.DEV && !t?.closest("input, textarea, [contenteditable]")) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("contextmenu", onMenu);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("contextmenu", onMenu);
    };
  }, [changeLayout]);

  const openFilesRef = useRef<(conn: ConnectionNode) => void>(() => {});
  openFilesRef.current = (conn: ConnectionNode) => {
    setView("connections");
    setPeek(false);
    if (dock.current) openConnection(dock.current, conn, { files: true });
    rememberOpened(conn.id);
  };

  const open = (conn: ConnectionNode) => {
    setView("connections");
    setPeek(false);
    if (dock.current) openConnection(dock.current, conn);
    rememberOpened(conn.id);
  };
  openDefaultShell.current = () => {
    if (shells?.length) open(localConnection(shells[0]));
  };

  /** A launch that arrived before the workspace was ready. */
  const pendingLaunch = useRef<Launch | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const launch = useRef<(l: Launch) => void>(() => {});
  launch.current = (l: Launch) => {
    if (!dock.current || !backend) {
      pendingLaunch.current = l;
      return;
    }
    if (l.kind === "error") {
      setNotice(l.message);
      return;
    }
    if (l.kind === "script") {
      setScriptsOpen(true);
      void scripts.start(l.path);
      return;
    }
    if (l.kind === "quick") {
      const conn = parseQuickConnect(l.text);
      if (conn) open(conn);
      else setNotice(`Couldn't read "${l.text}" as a connection`);
      return;
    }
    void backend.getTree().then((t) => {
      const conn = findConnection(t, l.id);
      if (conn) open(conn);
    });
  };
  // A script asking for what a terminal shows.
  useEffect(() => {
    if (!backend) return;
    return backend.onScreenRequest((r) => void backend.screenReply(r.id, screens.read(r.session)));
  }, [backend]);
  useEffect(() => {
    if (!backend) return;
    void backend.takeLaunch().then((l) => l && launch.current(l));
    return backend.onLaunch((l) => launch.current(l));
  }, [backend]);
  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(null), 8000);
    return () => window.clearTimeout(t);
  }, [notice]);

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
    <PrefsContext.Provider value={prefsValue}>
      <div className="app">
        <nav className="rail" aria-label="Views">
          <button
            className={view === "connections" ? "is-active" : ""}
            onClick={() => {
              setView("connections");
              if (!panel.pinned) setPeek((p) => !p);
            }}
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
          <button
            className={view === "settings" ? "is-active" : ""}
            onClick={() => setView("settings")}
            title="Settings"
            aria-label="Settings"
          >
            <SettingsIcon size={19} />
          </button>
        </nav>

        <ConnectionPanel
          layout={panel}
          peek={peek}
          onPeek={setPeek}
          header={
            <div className="brand">
              <span className="brand-mark" aria-hidden="true">
                <i />
                <i />
                <i />
                <i />
              </span>
              Linecard
            </div>
          }
          main={
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
                <button
                  className="icon-btn"
                  aria-label="Find a connection"
                  title="Find a connection (Ctrl+Shift+P)"
                  onClick={() => setPaletteOpen(true)}
                >
                  <Search size={16} />
                </button>
                <ShellMenu shells={shells} onOpen={(s) => open(localConnection(s))} />
                <WorkspaceMenu dock={dock} findConnection={(id) => findConnection(tree, id)} />
                <button
                  className={`icon-btn ${prefs.showButtonBar ? "is-toggled" : ""}`}
                  aria-pressed={prefs.showButtonBar}
                  aria-label="Button bar"
                  title="Button bar: saved commands, one click to send"
                  onClick={() => prefsValue.update({ showButtonBar: !prefs.showButtonBar })}
                >
                  <SquareTerminal size={16} />
                </button>
                <button
                  className={`icon-btn ${script.running ? "is-running" : ""}`}
                  aria-label={script.running ? `Scripts (${script.running} running)` : "Scripts"}
                  title={
                    script.running
                      ? `Scripts: ${script.running} running`
                      : "Scripts: automate sessions with Python"
                  }
                  onClick={() => setScriptsOpen(true)}
                >
                  <FileCode2 size={16} />
                </button>
                <BroadcastButton dock={dock} />
                <ZoomControl />
                <LayoutPicker value={layout} onChange={changeLayout} />
              </header>

              {prefs.showButtonBar && <ButtonBar dock={dockApi} tree={tree} />}

              <div className="content">
                <div className="workspace-wrap" hidden={view !== "connections"}>
                  <Workspace
                    theme={theme}
                    onOpenCount={setOpenCount}
                    onReady={(api) => {
                      dock.current = api;
                      setDockApi(api);
                      const waiting = pendingLaunch.current;
                      pendingLaunch.current = null;
                      if (waiting) window.setTimeout(() => launch.current(waiting), 0);
                    }}
                  />
                </div>
                {view === "devices" && <DevicesPage devices={devices} syncOn={sync.state !== "off"} />}
                {view === "settings" && <SettingsPage theme={pref} onTheme={setPref} />}
              </div>

              <footer className="statusbar">
                <SyncStatus sync={sync} />
                {mismatch && (
                  <button className="status-link warn" onClick={() => setView("devices")}>
                    Devices on different versions
                  </button>
                )}
                {notice && <span className="status-notice">{notice}</span>}
                <span className="status-spacer" />
                {update && (
                  <button className="status-link accent" onClick={() => setShowUpdate(true)}>
                    <Download size={13} /> Update {update.version} available
                  </button>
                )}
                <button
                  className="status-version"
                  title={`What's new in ${version}`}
                  onClick={() => void backend?.openUrl(changelogUrl(version))}
                >
                  v{version}
                </button>
                {quitting && (
                  <CloseConfirm
                    title="Quit Linecard?"
                    message={`${openCount} open ${openCount === 1 ? "connection closes" : "connections close"}.`}
                    confirmLabel="Close all and quit"
                    onConfirm={(dontAsk) => void quit(dontAsk)}
                    onCancel={() => setQuitting(false)}
                  />
                )}
                {update && showUpdate && (
                  <UpdateDialog update={update} current={version} onClose={() => setShowUpdate(false)} />
                )}
              </footer>
            </main>
          }
        >
          {backend && treeActions && prefsReady && (
            <ConnectionTree
              tree={tree}
              actions={treeActions}
              onOpen={open}
              focusRequest={focusRequest}
              onAck={(id) => backend.ackChange(id)}
              onAckAll={() => backend.ackAll()}
              onRevert={(op) => backend.revert(op)}
              onMove={(id, parent, index) => backend.moveNode(id, parent, index)}
              closedFolders={prefs.closedFolders}
              onClosedFolders={(closedFolders) => prefsValue.update({ closedFolders })}
            />
          )}
        </ConnectionPanel>

        {passwordFor && (
          <PasswordDialog connectionId={passwordFor.id} name={passwordFor.name} onClose={() => setPasswordFor(null)} />
        )}
        {importing && <ImportDialog onClose={() => setImporting(false)} onImported={() => setView("connections")} />}
        {scriptsOpen && <ScriptsDialog tree={tree} onClose={() => setScriptsOpen(false)} />}
        {paletteOpen && (
          <CommandPalette
            tree={tree}
            recent={prefs.recent}
            extra={shellPalette}
            parseQuick={parseQuickConnect}
            onOpen={open}
            onClose={() => setPaletteOpen(false)}
          />
        )}

        {folderSettings && (
          <FolderSettingsDialog
            folderId={folderSettings.id}
            parentId={folderSettings.parentId}
            path={folderPath(tree, folderSettings.id)}
            jumpHosts={sshConnections(tree)}
            onClose={() => setFolderSettings(null)}
          />
        )}

        {bulk && backend && (
          <BulkEditor
            drafts={bulk}
            jumpHosts={sshConnections(tree)}
            onSave={async (drafts) => {
              const failed: string[] = [];
              for (const d of drafts) {
                try {
                  await backend.saveConnection(d);
                } catch (e) {
                  failed.push(`${d.name}: ${e instanceof Error ? e.message : String(e)}`);
                }
              }
              if (failed.length)
                throw new Error(`Saved ${drafts.length - failed.length} of ${drafts.length}. ${failed.join("; ")}`);
            }}
            onClose={() => {
              const ids = bulk.flatMap((d) => (d.id ? [d.id] : []));
              setBulk(null);
              setFocusRequest({ id: ids[0], ids, n: Date.now() });
            }}
          />
        )}

        {editing && backend && (
          <ConnectionEditor
            initial={editing}
            location={folderPath(tree, editing.folderId)}
            onSave={async (d) => {
              saved.current = await backend.saveConnection(d);
              return saved.current;
            }}
            onClose={() => {
              const id = saved.current;
              saved.current = null;
              setEditing(null);
              if (id) setFocusRequest({ id, n: Date.now() });
            }}
            jumpHosts={sshConnections(tree)}
          />
        )}
      </div>
    </PrefsContext.Provider>
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
