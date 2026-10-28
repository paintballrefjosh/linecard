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

export type Parity = "none" | "odd" | "even" | "mark" | "space";
export type FlowControl = "none" | "software" | "hardware";

export interface SerialSettings {
  port: string;
  baud: number;
  dataBits: number;
  parity: Parity;
  stopBits: number;
  flow: FlowControl;
}

/** The connection editor's form. `id: null` creates a new connection.
 *  Empty `username` / `tabColor` mean "inherit from the folder". */
export interface ConnectionDraft {
  id: string | null;
  folderId: string | null;
  name: string;
  kind: ConnectionKind;
  host: string;
  port: number;
  serial: SerialSettings | null;
  username: string | null;
  tabColor: string | null;
  /** Id of an SSH connection to tunnel through; null = from the folder, or direct. */
  jumpHost: string | null;
  /** Session logging for this connection; null = from the folder or Settings. */
  log: boolean | null;
  notes: string;
}

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

export interface PromptField {
  label: string;
  /** false = secret (password) */
  echo: boolean;
}

/** The server's host key needs a decision (first connection, or it changed). */
export interface HostKeyEvent {
  event: "hostKey";
  host: string;
  port: number;
  algorithm: string;
  /** "SHA256:…" */
  fingerprint: string;
  /** Set when a different key is on record. */
  changed: { path: string; line: number } | null;
}

/** Credentials requested (password, keyboard-interactive, username). */
export interface AuthEvent {
  event: "auth";
  title: string;
  instructions: string;
  fields: PromptField[];
  /** Offer "Save password" with this initial state; null = don't offer. */
  remember: boolean | null;
}

export type SessionEvent =
  | { event: "connected" }
  | { event: "data"; data: number[] }
  | { event: "title"; title: string }
  | HostKeyEvent
  | AuthEvent
  | { event: "disconnected"; reason: string | null };

export type HostKeyDecision = "acceptAndSave" | "acceptOnce" | "reject";

export type PromptReply =
  | { type: "hostKey"; decision: HostKeyDecision }
  | { type: "fields"; values: string[]; remember: boolean }
  | { type: "cancel" };

export type LayoutId = "1" | "2v" | "2h" | "3c" | "1+2" | "4";

/** A password saved in the OS keychain (the secret itself never reaches the UI). */
export interface SavedPassword {
  key: string;
  /** e.g. "admin@10.0.0.1" */
  label: string;
  savedAt: number;
}

/** App-wide session logging defaults. */
export interface LoggingSettings {
  enabled: boolean;
  /** Folder, or a template like D:logs%host_%Y%m%d.log; null = default. */
  folder: string | null;
  timestamps: boolean;
  defaultFolder?: string | null;
}

export type ImportSourceId = "securecrt" | "putty" | "mremoteng" | "ssh_config";

export interface ImportSource {
  id: ImportSourceId;
  label: string;
  /** Default location; null for the registry (PuTTY). */
  path: string | null;
  found: boolean;
  hint: string;
}

export interface ImportPreview {
  summary: string;
  warnings: string[];
  rows: { folder: string; name: string; kind: ConnectionKind; endpoint: string; username: string | null }[];
}
