import type {
  ConnectionDraft,
  ConnectionNode,
  FolderSettings,
  Launch,
  Device,
  ImportPreview,
  ImportSource,
  ImportSourceId,
  Inherited,
  LoggingSettings,
  PromptReply,
  SavedPassword,
  SessionEvent,
  SyncConfig,
  SyncState,
  SyncTransport,
  TransferProtocol,
  TreeNode,
} from "./types";
import { mockBackend } from "./mock";
import type { HighlightProfile } from "./highlight";
import type { Preferences } from "./prefs";

export interface SessionHandle {
  write(data: string): void;
  resize(cols: number, rows: number): void;
  /** Answer a hostKey / auth event. */
  reply(r: PromptReply): void;
  /** BREAK on a serial line (password recovery, ROMMON). */
  sendBreak(): void;
  /** XMODEM/YMODEM on serial lines; progress arrives as transfer events. */
  startTransfer(send: boolean, protocol: TransferProtocol, path: string): Promise<void>;
  cancelTransfer(): void;
  close(): void;
}

/** Everything the UI needs from the Rust core. */
export interface Backend {
  appVersion(): Promise<string>;
  getTree(): Promise<TreeNode[]>;
  getDevices(): Promise<Device[]>;
  getSyncState(): Promise<SyncState>;
  /** null when sync is off. */
  getSyncConfig(): Promise<SyncConfig | null>;
  /** Start a new sync group (phrase null) or join one. Returns the phrase. */
  syncSetup(transport: SyncTransport, location: string, phrase: string | null): Promise<string>;
  /** The 24-word phrase, to add another computer. */
  syncPhrase(): Promise<string | null>;
  syncNow(): Promise<void>;
  /** Replace the group's phrase (to remove a computer). Returns the new one. */
  syncRotate(): Promise<string>;
  syncDisable(): Promise<void>;
  /** What the command line asked to open at startup (asked once). */
  takeLaunch(): Promise<Launch | null>;
  /** Launches (command line, links) after startup. */
  onLaunch(cb: (l: Launch) => void): () => void;
  /** Whether ssh:// and telnet:// links open here; null where unsupported. */
  linkHandlerStatus(): Promise<boolean | null>;
  setLinkHandler(on: boolean): Promise<void>;
  /** Fires when the sync status changes. */
  onSyncChanged(cb: () => void): () => void;
  ackChange(objectId: string): Promise<void>;
  ackAll(): Promise<void>;
  /** Revert one remote op; becomes a new op that syncs everywhere. */
  revert(opId: string): Promise<void>;
  moveNode(id: string, parentId: string | null, index: number): Promise<void>;
  /** Returns the new folder's id. */
  createFolder(parentId: string | null, name: string): Promise<string>;
  getConnection(id: string): Promise<ConnectionDraft>;
  /** What a connection in this folder inherits when it doesn't set a value. */
  inheritedSettings(folderId: string | null): Promise<Inherited>;
  /** Create (`draft.id` null) or update. Returns the connection's id. */
  saveConnection(draft: ConnectionDraft): Promise<string>;
  renameNode(id: string, name: string): Promise<void>;
  /** Deletes a connection, or a folder and everything in it. */
  deleteNode(id: string): Promise<void>;
  /** Returns the copy's id. */
  duplicateConnection(id: string): Promise<string>;
  /** This device's stored preferences (null on first run). */
  getPreferences(): Promise<Partial<Preferences> | null>;
  setPreferences(p: Preferences): Promise<void>;
  listSavedPasswords(): Promise<SavedPassword[]>;
  forgetSavedPassword(key: string): Promise<void>;
  importSources(): Promise<ImportSource[]>;
  importPreview(source: ImportSourceId, path: string | null): Promise<ImportPreview>;
  /** Returns the id of the new "Imported from …" folder. */
  importApply(source: ImportSourceId, path: string | null): Promise<string>;
  getFolderSettings(id: string): Promise<FolderSettings>;
  saveFolderSettings(id: string, settings: FolderSettings): Promise<void>;
  /** Keyword-highlight profiles (sorted by name) and the app-wide default. */
  getHighlight(): Promise<{ profiles: HighlightProfile[]; defaultId: string | null }>;
  /** The profile a connection uses (the default for quick connects). */
  effectiveHighlight(connectionId: string | null): Promise<string | null>;
  /** Create (empty id) or replace. Returns the id. */
  saveHighlightProfile(profile: HighlightProfile): Promise<string>;
  deleteHighlightProfile(id: string): Promise<void>;
  setDefaultHighlight(id: string): Promise<void>;
  /** SecureCRT keyword lists found on this computer. */
  securecrtKeywordFiles(): Promise<{ name: string; path: string }[]>;
  /** Import a SecureCRT keyword list as a new profile. Returns its id. */
  importHighlight(path: string): Promise<string>;
  getLogging(): Promise<LoggingSettings>;
  setLogging(l: LoggingSettings): Promise<void>;
  /** Serial ports present right now. */
  listSerialPorts(): Promise<{ name: string; description: string | null }[]>;
  /** Native folder picker; null if canceled. */
  pickFolder(title: string): Promise<string | null>;
  /** Native file picker; null if canceled. */
  pickFile(mode: "open" | "save", title: string): Promise<string | null>;
  clipboardRead(): Promise<string>;
  clipboardWrite(text: string): Promise<void>;
  /** Open a session. Saved connections are looked up by id; quick-connect
   *  entries (not in the tree) are opened from `kind` + `endpoint`. */
  openSession(
    conn: ConnectionNode,
    size: { cols: number; rows: number },
    onEvent: (e: SessionEvent) => void,
  ): Promise<SessionHandle>;
  /** Fires when the tree changes (local edit or remote sync). */
  onTreeChanged(cb: () => void): () => void;
}

const encoder = new TextEncoder();

async function tauriBackend(): Promise<Backend> {
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { getVersion } = await import("@tauri-apps/api/app");
  const clipboard = import("@tauri-apps/plugin-clipboard-manager");

  return {
    appVersion: () => getVersion(),
    getTree: () => invoke<TreeNode[]>("get_tree"),
    getDevices: () => invoke<Device[]>("get_devices"),
    getSyncState: () => invoke<SyncState>("get_sync_state"),
    getSyncConfig: () => invoke<SyncConfig | null>("get_sync_config"),
    syncSetup: (transport, location, phrase) => invoke<string>("sync_setup", { transport, location, phrase }),
    syncPhrase: () => invoke<string | null>("sync_phrase"),
    syncNow: () => invoke("sync_now"),
    syncRotate: () => invoke<string>("sync_rotate"),
    syncDisable: () => invoke("sync_disable"),
    takeLaunch: () => invoke<Launch | null>("take_launch"),
    onLaunch(cb) {
      const unlisten = listen<Launch>("launch", (e) => cb(e.payload));
      return () => void unlisten.then((u) => u());
    },
    linkHandlerStatus: () => invoke<boolean | null>("link_handler_status"),
    setLinkHandler: (on) => invoke("set_link_handler", { on }),
    onSyncChanged(cb) {
      const unlisten = listen("sync-changed", () => cb());
      return () => void unlisten.then((u) => u());
    },
    ackChange: (objectId) => invoke("ack_change", { objectId }),
    ackAll: () => invoke("ack_all"),
    revert: (opId) => invoke("revert_op", { opId }),
    moveNode: (id, parentId, index) => invoke("move_node", { id, parentId, index }),
    createFolder: (parentId, name) => invoke<string>("create_folder", { parentId, name }),
    getConnection: (id) => invoke<ConnectionDraft>("get_connection", { id }),
    inheritedSettings: (folderId) => invoke<Inherited>("inherited_settings", { folderId }),
    saveConnection: (draft) => invoke<string>("save_connection", { draft }),
    renameNode: (id, name) => invoke("rename_node", { id, name }),
    deleteNode: (id) => invoke("delete_node", { id }),
    duplicateConnection: (id) => invoke<string>("duplicate_connection", { id }),
    getPreferences: () => invoke<Partial<Preferences> | null>("get_preferences"),
    setPreferences: (preferences) => invoke("set_preferences", { preferences }),
    listSavedPasswords: () => invoke<SavedPassword[]>("list_saved_passwords"),
    forgetSavedPassword: (key) => invoke("forget_saved_password", { key }),
    importSources: () => invoke<ImportSource[]>("import_sources"),
    importPreview: (source, path) => invoke<ImportPreview>("import_preview", { source, path }),
    importApply: (source, path) => invoke<string>("import_apply", { source, path }),
    getFolderSettings: (id) => invoke<FolderSettings>("get_folder_settings", { id }),
    saveFolderSettings: (id, settings) => invoke("save_folder_settings", { id, settings }),
    getHighlight: () => invoke("get_highlight"),
    effectiveHighlight: (connectionId) => invoke<string | null>("effective_highlight", { connectionId }),
    saveHighlightProfile: (profile) => invoke<string>("save_highlight_profile", { profile }),
    deleteHighlightProfile: (id) => invoke("delete_highlight_profile", { id }),
    setDefaultHighlight: (id) => invoke("set_default_highlight", { id }),
    securecrtKeywordFiles: () => invoke("securecrt_keyword_files"),
    importHighlight: (path) => invoke<string>("import_highlight", { path }),
    getLogging: () => invoke<LoggingSettings>("get_logging"),
    setLogging: (logging) => invoke("set_logging", { logging }),
    listSerialPorts: () => invoke("list_serial_ports"),
    async pickFolder(title) {
      const dialog = await import("@tauri-apps/plugin-dialog");
      const picked = await dialog.open({ title, multiple: false, directory: true });
      return typeof picked === "string" ? picked : null;
    },
    async pickFile(mode, title) {
      const dialog = await import("@tauri-apps/plugin-dialog");
      const picked =
        mode === "open"
          ? await dialog.open({ title, multiple: false, directory: false })
          : await dialog.save({ title });
      return typeof picked === "string" ? picked : null;
    },
    // The clipboard plugin: WebView2 can refuse navigator.clipboard reads.
    clipboardRead: async () => (await clipboard).readText().catch(() => ""),
    clipboardWrite: async (text) => (await clipboard).writeText(text),
    async openSession(conn, size, onEvent) {
      const channel = new Channel<SessionEvent>();
      channel.onmessage = onEvent;
      const sessionId = await invoke<string>("open_session", {
        connectionId: conn.id,
        kind: conn.kind,
        endpoint: conn.endpoint,
        cols: size.cols,
        rows: size.rows,
        onEvent: channel,
      });
      return {
        write: (data) => void invoke("session_write", { sessionId, data: Array.from(encoder.encode(data)) }),
        resize: (cols, rows) => void invoke("session_resize", { sessionId, cols, rows }),
        reply: (reply) => void invoke("session_reply", { sessionId, reply }),
        sendBreak: () => void invoke("session_break", { sessionId }),
        startTransfer: (send, protocol, path) => invoke("session_transfer", { sessionId, send, protocol, path }),
        cancelTransfer: () => void invoke("session_cancel_transfer", { sessionId }),
        close: () => void invoke("session_close", { sessionId }),
      };
    },
    onTreeChanged(cb) {
      const unlisten = listen("tree-changed", () => cb());
      return () => void unlisten.then((u) => u());
    },
  };
}

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

let backend: Promise<Backend> | null = null;

/** Real backend inside the desktop app; an in-memory mock in a plain browser
 *  (`npm run dev`), so the UI can be developed without building Rust. */
export function getBackend(): Promise<Backend> {
  backend ??= isTauri ? tauriBackend() : Promise.resolve(mockBackend());
  return backend;
}
