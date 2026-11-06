import type {
  ConnectionDraft,
  ConnectionNode,
  FolderSettings,
  Launch,
  LocalShell,
  PasswordSync,
  Device,
  ImportPreview,
  ImportSource,
  ImportSourceId,
  Inherited,
  LoggingSettings,
  PromptReply,
  PasswordTarget,
  RdpShare,
  SavedPassword,
  SftpEntry,
  ScriptEvent,
  ScriptFile,
  ScriptStatus,
  Snippet,
  SessionEvent,
  GfxEvent,
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
  /** The backend's id for this session. */
  readonly id: string;
  /** This tab is the one in use now (scripts act on it by default). */
  focus(): void;
  close(): void;
}

/** A script that is running. */
export interface ScriptRun {
  stop(): void;
  /** Answer the script's question (null: cancel). */
  answer(askId: number, value: string | null): void;
}

/** An SFTP session. Paths on the server are always "/"-separated. */
export interface SftpHandle {
  list(path: string): Promise<SftpEntry[]>;
  /** The absolute form of a path (resolves ".."). */
  resolve(path: string): Promise<string>;
  mkdir(path: string): Promise<void>;
  rename(from: string, to: string): Promise<void>;
  /** Delete files and folders (folders with all their contents). */
  remove(paths: string[]): Promise<void>;
  /** Copy remote files or folders into a local folder. `onProgress` gets bytes so far. */
  download(remote: string[], destFolder: string, resume: boolean, onProgress: (bytes: number) => void): Promise<void>;
  /** Copy local files or folders into a remote folder. */
  upload(local: string[], remoteDir: string, resume: boolean, onProgress: (bytes: number) => void): Promise<void>;
  /** Stop the transfer in progress (it fails with "canceled"; what was copied stays, so it can be resumed). */
  cancel(): void;
  /** Set permission bits (e.g. 0o644); `recursive` also covers everything inside a folder. */
  chmod(path: string, mode: number, recursive: boolean): Promise<void>;
  reply(r: PromptReply): void;
  close(): void;
}

/** A VNC or RDP session. Buttons: left 1, middle 2, right 4. */
export interface GfxHandle {
  pointer(x: number, y: number, buttons: number): void;
  wheel(x: number, y: number, dx: number, dy: number): void;
  /** `code` and `key` as in KeyboardEvent. */
  key(code: string, key: string, down: boolean): void;
  /** Type text (paste). */
  text(text: string): void;
  /** Offer the local clipboard to the server (VNC). */
  clipboard(text: string): void;
  reply(r: PromptReply): void;
  resize(width: number, height: number): void;
  close(): void;
}

/** Binary frame: x, y, width, height (u16 little endian), then RGBA; or
 *  sound: 0xFFFF, channels (u16), rate (u32), then 16-bit samples. */
function decodeFrame(buf: ArrayBuffer): GfxEvent {
  const v = new DataView(buf);
  // Sound shares the channel with frames; 0xFFFF where x would be marks it.
  if (v.getUint16(0, true) === 0xffff)
    return { event: "audio", channels: v.getUint16(2, true), rate: v.getUint32(4, true), pcm: new Uint8Array(buf, 8) };
  return {
    event: "frame",
    x: v.getUint16(0, true),
    y: v.getUint16(2, true),
    width: v.getUint16(4, true),
    height: v.getUint16(6, true),
    rgba: new Uint8ClampedArray(buf, 8),
  };
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
  /** `account`: a private relay's access key. */
  syncSetup(
    transport: SyncTransport,
    location: string,
    phrase: string | null,
    account?: string | null,
  ): Promise<string>;
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
  /** Whether `linecard` is on PATH (on: null = can't change it here). */
  cliPathStatus(): Promise<{ on: boolean | null; manual: string | null }>;
  setCliPath(on: boolean): Promise<void>;
  /** Fires when the sync status changes. */
  /** A script wants the text on a terminal screen. */
  onScreenRequest(cb: (req: { id: number; session: string }) => void): () => void;
  screenReply(id: number, text: string | null): Promise<void>;
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
  /** Linecard's own error log, as text. */
  /** Whether Python scripts can run, and where they're kept. */
  scriptStatus(): Promise<ScriptStatus>;
  listScripts(): Promise<ScriptFile[]>;
  /** Create a script from a template; returns its path. */
  newScript(name: string): Promise<string>;
  openScriptsFolder(): Promise<void>;
  /** Run a script. Output, questions and the end arrive on `onEvent`. */
  runScript(path: string, onEvent: (e: ScriptEvent) => void): Promise<ScriptRun>;
  debugLog(): Promise<string>;
  openDebugLogFolder(): Promise<void>;
  /** Every button-bar snippet (for the manager). */
  listSnippets(): Promise<Snippet[]>;
  /** The snippets for a connection's button bar (null: app-wide ones only). */
  snippetsFor(connectionId: string | null): Promise<Snippet[]>;
  /** Create (empty id) or replace a snippet; returns its id. */
  saveSnippet(s: Snippet): Promise<string>;
  deleteSnippet(id: string): Promise<void>;
  reorderSnippets(ids: string[]): Promise<void>;
  /** Add a line to the debug log. */
  logDebug(area: string, message: string): void;
  /** Which user name (if any) a connection's saved password is keyed by. */
  passwordTarget(connectionId: string): Promise<PasswordTarget>;
  /** Save a password for a connection without logging in first. */
  setConnectionPassword(connectionId: string, user: string | null, password: string): Promise<void>;
  getPasswordSync(): Promise<PasswordSync>;
  /** Set (first computer) or enter (the others) the vault passphrase. */
  enablePasswordSync(passphrase: string): Promise<void>;
  disablePasswordSync(): Promise<void>;
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
  /** Whether this system can share a folder with a remote computer yet. */
  rdpShareSupported(): Promise<boolean>;
  /** The folder an RDP connection shares, if any. */
  getRdpShare(connectionId: string): Promise<RdpShare | null>;
  /** Share a folder with an RDP connection, or stop sharing with null. */
  setRdpShare(connectionId: string, share: RdpShare | null): Promise<void>;
  pickFolder(title: string): Promise<string | null>;
  /** Native file picker; null if canceled. */
  pickFile(mode: "open" | "save", title: string): Promise<string | null>;
  /** Native picker for several files; empty if canceled. */
  pickFiles(title: string): Promise<string[]>;
  /** Staging for files dropped onto the SFTP browser: a temporary folder
   *  they are written into (the window gives no paths for dropped files). */
  stageBegin(): Promise<string>;
  stageMkdir(dir: string, rel: string): Promise<void>;
  stageWrite(dir: string, rel: string, offset: number, bytes: Uint8Array): Promise<void>;
  stageEnd(dir: string): Promise<void>;
  /** Open a web page in the default browser. */
  openUrl(url: string): Promise<void>;
  clipboardRead(): Promise<string>;
  clipboardWrite(text: string): Promise<void>;
  /** Shells this computer can open in a tab (finding WSL distributions can
   *  take a moment). */
  localShells(): Promise<LocalShell[]>;
  /** Open a session. Saved connections are looked up by id; quick-connect
   *  entries (not in the tree) are opened from `kind` + `endpoint`; local
   *  shells by the shell id in `endpoint`. */
  openSession(
    conn: ConnectionNode,
    size: { cols: number; rows: number },
    onEvent: (e: SessionEvent) => void,
  ): Promise<SessionHandle>;
  /** Open a VNC or RDP session sized for the tab. */
  openGfxSession(
    conn: ConnectionNode,
    size: { width: number; height: number },
    onEvent: (e: GfxEvent) => void,
  ): Promise<GfxHandle>;
  /** Start an SFTP session to an SSH connection. The handle can answer
   *  login prompts at once; `opened` resolves with the login folder. */
  openSftp(conn: ConnectionNode, onEvent: (e: SessionEvent) => void): { handle: SftpHandle; opened: Promise<string> };
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
    syncSetup: (transport, location, phrase, account) =>
      invoke<string>("sync_setup", { transport, location, phrase, account: account ?? null }),
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
    cliPathStatus: () => invoke("cli_path_status"),
    setCliPath: (on) => invoke("set_cli_path", { on }),
    onScreenRequest(cb) {
      const unlisten = listen<{ id: number; session: string }>("script-screen", (e) => cb(e.payload));
      return () => void unlisten.then((u) => u());
    },
    screenReply: (id, text) => invoke("script_screen_reply", { id, text }),
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
    scriptStatus: () => invoke<ScriptStatus>("script_status"),
    listScripts: () => invoke<ScriptFile[]>("script_list"),
    newScript: (name) => invoke<string>("script_new", { name }),
    openScriptsFolder: () => invoke("open_scripts_folder"),
    async runScript(path, onEvent) {
      const channel = new Channel<ScriptEvent>();
      channel.onmessage = onEvent;
      const runId = await invoke<number>("script_run", { path, onEvent: channel });
      return {
        stop: () => void invoke("script_stop", { runId }),
        answer: (askId, value) => void invoke("script_answer", { runId, askId, value }),
      };
    },
    debugLog: () => invoke<string>("debug_log_text"),
    openDebugLogFolder: () => invoke("open_debug_log_folder"),
    listSnippets: () => invoke<Snippet[]>("list_snippets"),
    snippetsFor: (connectionId) => invoke<Snippet[]>("snippets_for", { connectionId }),
    saveSnippet: (snippet) => invoke<string>("save_snippet", { snippet }),
    deleteSnippet: (id) => invoke("delete_snippet", { id }),
    reorderSnippets: (ids) => invoke("reorder_snippets", { ids }),
    logDebug: (area, message) => void invoke("log_debug", { area, message }).catch(() => {}),
    passwordTarget: (connectionId) => invoke<PasswordTarget>("password_target", { connectionId }),
    setConnectionPassword: (connectionId, user, password) =>
      invoke("set_connection_password", { connectionId, user, password }),
    getPasswordSync: () => invoke<PasswordSync>("get_password_sync"),
    enablePasswordSync: (passphrase) => invoke("enable_password_sync", { passphrase }),
    disablePasswordSync: () => invoke("disable_password_sync"),
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
    rdpShareSupported: () => invoke<boolean>("rdp_share_supported"),
    getRdpShare: (connectionId) => invoke<RdpShare | null>("get_rdp_share", { connectionId }),
    setRdpShare: (connectionId, share) => invoke<void>("set_rdp_share", { connectionId, share }),
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
    async pickFiles(title) {
      const dialog = await import("@tauri-apps/plugin-dialog");
      const picked = await dialog.open({ title, multiple: true, directory: false });
      return Array.isArray(picked) ? picked : [];
    },
    stageBegin: () => invoke<string>("stage_begin"),
    stageMkdir: (dir, rel) => invoke("stage_mkdir", { dir, rel }),
    stageWrite: (dir, rel, offset, bytes) =>
      invoke("stage_write", bytes, {
        headers: { "x-dir": encodeURIComponent(dir), "x-rel": encodeURIComponent(rel), "x-offset": String(offset) },
      }),
    stageEnd: (dir) => invoke("stage_end", { dir }),
    openUrl: async (url) => (await import("@tauri-apps/plugin-opener")).openUrl(url),
    // The clipboard plugin: WebView2 can refuse navigator.clipboard reads.
    clipboardRead: async () => (await clipboard).readText().catch(() => ""),
    clipboardWrite: async (text) => (await clipboard).writeText(text),
    localShells: () => invoke<LocalShell[]>("local_shells"),
    async openSession(conn, size, onEvent) {
      // Output arrives as raw bytes; every other event as JSON.
      const channel = new Channel<unknown>();
      channel.onmessage = (m) => {
        if (m instanceof ArrayBuffer) onEvent({ event: "data", data: new Uint8Array(m) });
        else if (ArrayBuffer.isView(m))
          onEvent({ event: "data", data: new Uint8Array(m.buffer, m.byteOffset, m.byteLength) });
        else onEvent(m as SessionEvent);
      };
      // A local shell's endpoint is its id from localShells().
      const sessionId =
        conn.kind === "local"
          ? await invoke<string>("open_local_session", {
              shellId: conn.endpoint,
              cols: size.cols,
              rows: size.rows,
              onEvent: channel,
            })
          : await invoke<string>("open_session", {
              connectionId: conn.id,
              kind: conn.kind,
              endpoint: conn.endpoint,
              cols: size.cols,
              rows: size.rows,
              onEvent: channel,
            });
      return {
        id: sessionId,
        write: (data) => void invoke("session_write", encoder.encode(data), { headers: { "x-session": sessionId } }),
        resize: (cols, rows) => void invoke("session_resize", { sessionId, cols, rows }),
        reply: (reply) => void invoke("session_reply", { sessionId, reply }),
        sendBreak: () => void invoke("session_break", { sessionId }),
        startTransfer: (send, protocol, path) => invoke("session_transfer", { sessionId, send, protocol, path }),
        cancelTransfer: () => void invoke("session_cancel_transfer", { sessionId }),
        focus: () => void invoke("session_focus", { sessionId }),
        close: () => void invoke("session_close", { sessionId }),
      };
    },
    async openGfxSession(conn, size, onEvent) {
      const channel = new Channel<unknown>();
      channel.onmessage = (m) => {
        if (m instanceof ArrayBuffer) onEvent(decodeFrame(m));
        else if (ArrayBuffer.isView(m))
          onEvent(decodeFrame(m.buffer.slice(m.byteOffset, m.byteOffset + m.byteLength) as ArrayBuffer));
        else onEvent(m as GfxEvent);
      };
      const sessionId = await invoke<string>("open_gfx_session", {
        connectionId: conn.id,
        kind: conn.kind,
        endpoint: conn.endpoint,
        width: size.width,
        height: size.height,
        onEvent: channel,
      });
      return {
        pointer: (x, y, buttons) => void invoke("gfx_pointer", { sessionId, x, y, buttons }),
        wheel: (x, y, dx, dy) => void invoke("gfx_wheel", { sessionId, x, y, dx, dy }),
        key: (code, key, down) => void invoke("gfx_key", { sessionId, code, key, down }),
        text: (text) => void invoke("gfx_text", { sessionId, text }),
        clipboard: (text) => void invoke("gfx_clipboard", { sessionId, text }),
        reply: (reply) => void invoke("gfx_reply", { sessionId, reply }),
        resize: (width, height) => void invoke("gfx_resize", { sessionId, width, height }),
        close: () => void invoke("gfx_close", { sessionId }),
      };
    },
    openSftp(conn, onEvent) {
      const sessionId = crypto.randomUUID();
      const channel = new Channel<SessionEvent>();
      channel.onmessage = onEvent;
      const opened = invoke<string>("sftp_open", {
        sessionId,
        connectionId: conn.id,
        endpoint: conn.endpoint,
        onEvent: channel,
      });
      const progress = (cb: (n: number) => void) => {
        const ch = new Channel<number>();
        ch.onmessage = cb;
        return ch;
      };
      const handle: SftpHandle = {
        list: (path) => invoke<SftpEntry[]>("sftp_list", { sessionId, path }),
        resolve: (path) => invoke<string>("sftp_resolve", { sessionId, path }),
        mkdir: (path) => invoke("sftp_mkdir", { sessionId, path }),
        rename: (from, to) => invoke("sftp_rename", { sessionId, from, to }),
        remove: (paths) => invoke("sftp_remove", { sessionId, paths }),
        download: (remote, dest, resume, onProgress) =>
          invoke("sftp_download", { sessionId, remote, dest, resume, onProgress: progress(onProgress) }),
        upload: (local, dir, resume, onProgress) =>
          invoke("sftp_upload", { sessionId, local, dir, resume, onProgress: progress(onProgress) }),
        cancel: () => void invoke("sftp_cancel", { sessionId }).catch(() => {}),
        chmod: (path, mode, recursive) => invoke("sftp_chmod", { sessionId, path, mode, recursive }),
        reply: (reply) => void invoke("sftp_reply", { sessionId, reply }).catch(() => {}),
        close: () => void invoke("sftp_close", { sessionId }).catch(() => {}),
      };
      return { handle, opened };
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
