import type { ConnectionNode, Device, SessionEvent, SyncState, TreeNode } from "./types";
import { mockBackend } from "./mock";

export interface SessionHandle {
  write(data: string): void;
  resize(cols: number, rows: number): void;
  close(): void;
}

/** Everything the UI needs from the Rust core. */
export interface Backend {
  appVersion(): Promise<string>;
  getTree(): Promise<TreeNode[]>;
  getDevices(): Promise<Device[]>;
  getSyncState(): Promise<SyncState>;
  ackChange(objectId: string): Promise<void>;
  ackAll(): Promise<void>;
  /** Revert one remote op; becomes a new op that syncs everywhere. */
  revert(opId: string): Promise<void>;
  moveNode(id: string, parentId: string | null, index: number): Promise<void>;
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

  return {
    appVersion: () => getVersion(),
    getTree: () => invoke<TreeNode[]>("get_tree"),
    getDevices: () => invoke<Device[]>("get_devices"),
    getSyncState: () => invoke<SyncState>("get_sync_state"),
    ackChange: (objectId) => invoke("ack_change", { objectId }),
    ackAll: () => invoke("ack_all"),
    revert: (opId) => invoke("revert_op", { opId }),
    moveNode: (id, parentId, index) => invoke("move_node", { id, parentId, index }),
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
