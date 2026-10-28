// Shapes shared with the Rust backend (serde JSON). Keep in sync with
// app/src-tauri/src/dto.rs.

export type ConnectionKind = "ssh" | "telnet" | "serial" | "rdp" | "vnc";

export type ChangeKind = "added" | "edited" | "moved" | "deleted" | "restored" | "reverted";

export interface FieldDiff {
  field: string;
  before: string | null;
  after: string | null;
}

export interface ChangeEntry {
  opId: string;
  kind: ChangeKind;
  device: string;
  /** Unix ms */
  at: number;
  fields: FieldDiff[];
}

export interface PendingChange {
  kind: ChangeKind;
  entries: ChangeEntry[];
}

export interface FolderNode {
  type: "folder";
  id: string;
  name: string;
  color?: string;
  change?: PendingChange;
  children: TreeNode[];
}

export interface ConnectionNode {
  type: "connection";
  id: string;
  name: string;
  kind: ConnectionKind;
  /** "10.0.0.1:22" or "COM3 9600" */
  endpoint: string;
  tabColor?: string;
  change?: PendingChange;
}

export type TreeNode = FolderNode | ConnectionNode;

export interface Device {
  id: string;
  name: string;
  os: string;
  appVersion: string;
  protocolVersion: number;
  lastSeen: number;
  transport: "relay" | "git" | "lan";
  pendingOps: number;
  isThisDevice: boolean;
}

export type SyncState =
  | { state: "off" }
  | { state: "synced"; transport: string; at: number }
  | { state: "syncing"; transport: string }
  | { state: "offline"; pending: number }
  | { state: "error"; message: string };

export type SessionEvent =
  | { event: "connected" }
  | { event: "data"; data: number[] }
  | { event: "title"; title: string }
  | { event: "disconnected"; reason: string | null };

export type LayoutId = "1" | "2v" | "2h" | "3c" | "1+2" | "4";
