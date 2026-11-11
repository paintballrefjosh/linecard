// Shapes shared with the Rust backend (serde JSON). Keep in sync with
// app/src-tauri/src/dto.rs.

/** "local" is a shell on this computer: opened in a tab, never saved or synced. */
export type ConnectionKind = "ssh" | "telnet" | "serial" | "rdp" | "vnc" | "local";

/** A shell this computer can open in a tab ("pwsh", "wsl:Ubuntu", "/bin/zsh"…). */
export interface LocalShell {
  id: string;
  name: string;
}

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
/** A folder of this computer shared with an RDP connection (kept on this computer only). */
export interface RdpShare {
  path: string;
  writable: boolean;
}

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
  /** Keyword-highlight profile id; null = from the folder or Settings. */
  highlight: string | null;
  /** Named credential id; null = from the folder. */
  credential: string | null;
  /** Logon actions; null = from the folder, [] = none here. */
  logonActions: LogonStep[] | null;
  /** Lines of scrollback; null = from the folder or Settings. */
  scrollback: number | null;
  /** SSH port forwards, started with the session. */
  forwards: PortForward[];
  notes: string;
}

export interface Device {
  id: string;
  name: string;
  os: string;
  appVersion: string;
  protocolVersion: number;
  lastSeen: number;
  /** "folder", "relay", or "" when sync is off. */
  transport: string;
  pendingOps: number;
  isThisDevice: boolean;
}

export type SyncTransport = "folder" | "relay" | "github" | "lan";

export interface SyncConfig {
  transport: SyncTransport;
  /** Folder path or relay URL. */
  location: string;
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
  /** Raw bytes as the remote sent them. */
  | { event: "data"; data: Uint8Array }
  | { event: "title"; title: string }
  | { event: "forwards"; forwards: ForwardState[] }
  | HostKeyEvent
  | AuthEvent
  | TransferEvent
  | ZmodemEvent
  | { event: "disconnected"; reason: string | null };

export interface PasswordSync {
  /** Sync is set up (password sync needs it). */
  available: boolean;
  /** On for this computer. */
  enabled: boolean;
  /** A vault passphrase has been set (here or on another computer). */
  exists: boolean;
  /** Passwords in the synced vault. */
  count: number;
}

/** Something to open from the command line or a link. */
export type Launch =
  | { kind: "saved"; id: string }
  | { kind: "quick"; text: string }
  | { kind: "script"; path: string }
  | { kind: "error"; message: string };

/** local = -L, remote = -R, dynamic = -D (SOCKS). */
export type ForwardKind = "local" | "remote" | "dynamic";

export interface PortForward {
  kind: ForwardKind;
  /** Empty = loopback only. */
  bindAddress: string;
  bindPort: number;
  /** Destination (unused for dynamic). */
  host: string;
  port: number;
}

export interface ForwardState {
  /** e.g. "L 127.0.0.1:8080 → 10.0.0.5:80" */
  label: string;
  /** null while working. */
  error: string | null;
}

/** Events from a VNC or RDP session. Frames carry RGBA pixels. */
export type GfxEvent =
  | { event: "frame"; x: number; y: number; width: number; height: number; rgba: Uint8ClampedArray<ArrayBuffer> }
  | { event: "connected"; width: number; height: number; name: string }
  | { event: "resized"; width: number; height: number }
  | { event: "bell" }
  | { event: "clipboard"; text: string }
  | {
      event: "cursor";
      shape: "default" | "hidden" | "image";
      width: number;
      height: number;
      hotX: number;
      hotY: number;
      rgba: string;
    }
  | { event: "audio"; channels: number; rate: number; pcm: Uint8Array<ArrayBuffer> }
  | { event: "disconnected"; reason: string | null }
  | HostKeyEvent
  | AuthEvent;

export type TransferProtocol = "xmodem" | "xmodem1k" | "ymodem";

/** The server started `rz` (upload: we pick a file to send) or `sz` (we pick where to save). */
export interface ZmodemEvent {
  event: "zmodem";
  upload: boolean;
}

/** XMODEM/YMODEM/ZMODEM progress. */
export interface TransferEvent {
  event: "transfer";
  file: string;
  sending: boolean;
  done: number;
  total: number | null;
  finished: boolean;
  error: string | null;
}

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

export type ImportSourceId = "securecrt" | "putty" | "superputty" | "mremoteng" | "ssh_config";

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

/** A value a connection inherits, and where it comes from ("/DC1" or "Settings"). */
export interface InheritedValue {
  value: string;
  from: string;
}

export interface Inherited {
  username: InheritedValue | null;
  tabColor: InheritedValue | null;
  /** The jump host's name. */
  jumpHost: InheritedValue | null;
  /** "on" | "off" */
  log: InheritedValue | null;
  /** The keyword-highlight profile's name. */
  highlight: InheritedValue | null;
  /** The named credential's name. */
  credential: InheritedValue | null;
  /** What the inherited logon actions send, e.g. "enable, terminal length 0". */
  logonActions: InheritedValue | null;
  /** Lines of scrollback, e.g. "2500". */
  scrollback: InheritedValue | null;
}

/** What a folder passes down to its folders and connections; null = inherit. */
export interface FolderSettings {
  username: string | null;
  tabColor: string | null;
  jumpHost: string | null;
  log: boolean | null;
  highlight: string | null;
  credential: string | null;
  logonActions: LogonStep[] | null;
  scrollback: number | null;
}

/** Fewest and most lines of scrollback a terminal can keep. */
export const SCROLLBACK_MIN = 0;
export const SCROLLBACK_MAX = 100_000;
/** Scrollback when nothing sets it. */
export const SCROLLBACK_DEFAULT = 2500;

/** What a saved password for a connection is keyed by. */
export interface PasswordTarget {
  /** The user name the connection logs in as, if it has one. */
  user: string | null;
  /** SSH and RDP passwords are saved per user name; VNC's has none. */
  needsUser: boolean;
  /** A password is already saved. */
  saved: boolean;
}

/** A button-bar command. */
export interface Snippet {
  id: string;
  name: string;
  text: string;
  /** The folder it belongs to; null = every connection. */
  folder: string | null;
  order: number;
  /** Press Enter after the text. */
  enter: boolean;
}

/** One entry in an SFTP folder listing. */
export interface SftpEntry {
  name: string;
  kind: "file" | "dir" | "link" | "other";
  size: number;
  /** Seconds since the Unix epoch. */
  modified: number | null;
  /** Unix permission bits. */
  mode: number | null;
}

/** Whether Python scripts can run on this computer. */
export interface ScriptStatus {
  /** e.g. "3.11.9"; null when no usable Python was found. */
  version: string | null;
  problem: string | null;
  /** Where the scripts live. */
  folder: string;
}

export interface ScriptFile {
  name: string;
  path: string;
  modifiedMs: number;
}

/** What a running script reports. */
export type ScriptEvent =
  | { event: "output"; stderr: boolean; text: string }
  | { event: "ask"; id: number; prompt: string; secret: boolean }
  | { event: "finished"; code: number | null; stopped: boolean };

/** A user name and password many connections share. The password stays in the keychain. */
export interface Credential {
  id: string;
  name: string;
  username: string;
  /** A password is saved for it on this computer. */
  saved: boolean;
  /** Connections that log in with it (their own choice or inherited). */
  usedBy: number;
}

/** Creating (empty id) or changing a credential; an empty password keeps the saved one. */
export interface CredentialDraft {
  id: string;
  name: string;
  username: string;
  password: string | null;
}

/** After connecting: wait for some text, then send a line (and Enter). */
export interface LogonStep {
  /** Empty = send straight away. */
  expect: string;
  send: string;
  /** Send this credential's saved password instead of `send`. */
  passwordOf?: string | null;
}
