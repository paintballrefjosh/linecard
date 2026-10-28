// In-browser stand-in for the Rust backend, used by `npm run dev`.
// It demonstrates every highlight state and simulates a remote change.

import type { Backend, SessionHandle } from "./api";
import type {
  ConnectionDraft,
  ConnectionNode,
  Device,
  FolderNode,
  LoggingSettings,
  PendingChange,
  SerialSettings,
  TreeNode,
} from "./types";

const min = 60_000;
const now = () => Date.now();

function change(
  kind: PendingChange["kind"],
  device: string,
  agoMin: number,
  fields: PendingChange["entries"][0]["fields"] = [],
): PendingChange {
  return {
    kind,
    entries: [{ opId: `op-${Math.random().toString(36).slice(2, 9)}`, kind, device, at: now() - agoMin * min, fields }],
  };
}

function seed(): TreeNode[] {
  const conn = (
    id: string,
    name: string,
    kind: ConnectionNode["kind"],
    endpoint: string,
    extra: Partial<ConnectionNode> = {},
  ): ConnectionNode => ({
    type: "connection",
    id,
    name,
    kind,
    endpoint,
    ...extra,
  });
  const folder = (id: string, name: string, children: TreeNode[], extra: Partial<FolderNode> = {}): FolderNode => ({
    type: "folder",
    id,
    name,
    children,
    ...extra,
  });
  return [
    folder("dc1", "DC1 Fort Worth", [
      folder("dc1-spine", "Spine", [
        conn("spine01", "spine01", "ssh", "10.10.0.1:22", { tabColor: "#d64545" }),
        conn("spine02", "spine02", "ssh", "10.10.0.2:22", {
          tabColor: "#d64545",
          change: change("edited", "Desktop-Home", 4, [{ field: "host", before: "10.10.0.12", after: "10.10.0.2" }]),
        }),
      ]),
      folder("dc1-leaf", "Leaf", [
        conn("leaf01", "leaf01", "ssh", "10.10.1.1:22"),
        conn("leaf02", "leaf02", "ssh", "10.10.1.2:22"),
        conn("leaf03", "leaf03", "ssh", "10.10.1.3:22", { change: change("added", "Desktop-Home", 2) }),
      ]),
      folder("dc1-oob", "Out-of-band", [
        conn("cs1-p5", "cs1 port 5 · spine01 console", "telnet", "10.10.99.5:2005"),
        conn("cs1-p6", "cs1 port 6 · spine02 console", "telnet", "10.10.99.5:2006"),
        conn("usb-con", "USB console", "serial", "COM3 9600", {
          change: change("deleted", "Laptop-Win11", 11),
        }),
      ]),
    ]),
    folder("lab", "Lab", [
      conn("lab-jump", "jump-win", "rdp", "lab-jump.corp:3389", { tabColor: "#3fa35b" }),
      conn("lab-kvm", "kvm-switch", "vnc", "10.20.0.50:5900", {
        change: change("moved", "Laptop-Win11", 26, [{ field: "folder", before: "/DC1 Fort Worth", after: "/Lab" }]),
      }),
      conn("lab-r1", "r1", "telnet", "10.20.0.11:23", { tabColor: "#3fa35b" }),
    ]),
    conn("home-pi", "home-pi", "ssh", "192.168.1.20:22"),
  ];
}

function walk(nodes: TreeNode[], fn: (n: TreeNode, siblings: TreeNode[]) => boolean | void): boolean {
  for (const n of nodes) {
    if (fn(n, nodes)) return true;
    if (n.type === "folder" && walk(n.children, fn)) return true;
  }
  return false;
}

function find(nodes: TreeNode[], id: string): { node: TreeNode; siblings: TreeNode[] } | null {
  let hit: { node: TreeNode; siblings: TreeNode[] } | null = null;
  walk(nodes, (n, s) => {
    if (n.id === id) {
      hit = { node: n, siblings: s };
      return true;
    }
  });
  return hit;
}

export function mockBackend(): Backend {
  let tree = seed();
  const listeners = new Set<() => void>();
  const emit = () => listeners.forEach((l) => l());

  // A remote device adds a connection shortly after launch.
  setTimeout(() => {
    const leaf = find(tree, "dc1-leaf");
    if (leaf?.node.type === "folder") {
      leaf.node.children.push({
        type: "connection",
        id: "leaf04",
        name: "leaf04",
        kind: "ssh",
        endpoint: "10.10.1.4:22",
        change: change("added", "Laptop-Win11", 0),
      });
      emit();
    }
  }, 12_000);

  const devices: Device[] = [
    {
      id: "d1",
      name: "Workstation",
      os: "Windows 11",
      appVersion: "0.1.0",
      protocolVersion: 1,
      lastSeen: now(),
      transport: "relay",
      pendingOps: 0,
      isThisDevice: true,
    },
    {
      id: "d2",
      name: "Desktop-Home",
      os: "Windows 11",
      appVersion: "0.1.0",
      protocolVersion: 1,
      lastSeen: now() - 2 * min,
      transport: "relay",
      pendingOps: 0,
      isThisDevice: false,
    },
    {
      id: "d3",
      name: "Laptop-Win11",
      os: "Windows 11",
      appVersion: "0.0.9",
      protocolVersion: 1,
      lastSeen: now() - 9 * min,
      transport: "relay",
      pendingOps: 3,
      isThisDevice: false,
    },
    {
      id: "d4",
      name: "MacBook",
      os: "macOS 26",
      appVersion: "0.1.0",
      protocolVersion: 1,
      lastSeen: now() - 40 * 24 * 60 * min,
      transport: "git",
      pendingOps: 0,
      isThisDevice: false,
    },
  ];

  const clearChange = (id: string) => {
    const hit = find(tree, id);
    if (hit) delete hit.node.change;
  };

  /** Endpoints whose host key was saved this run (the mock's known_hosts). */
  const trustedHosts = new Set<string>();
  /** Used when the browser denies clipboard access (e.g. automated previews). */
  let memClipboard = "";
  let logging: LoggingSettings = { enabled: false, folder: null, timestamps: false };
  /** login label -> saved-at, standing in for the OS keychain. */
  const savedPasswords = new Map<string, number>();

  // Fields the tree nodes don't carry, as the Rust side stores them.
  const extra = new Map<
    string,
    {
      username: string | null;
      notes: string;
      serial: SerialSettings | null;
      jumpHost: string | null;
      log: boolean | null;
    }
  >();
  const newId = () => `m-${Math.random().toString(36).slice(2, 9)}`;
  const childrenOf = (parentId: string | null): TreeNode[] => {
    const hit = parentId ? find(tree, parentId) : null;
    return hit?.node.type === "folder" ? hit.node.children : tree;
  };
  const parentOf = (id: string): string | null => {
    let parent: string | null = null;
    walk(tree, (n) => {
      if (n.type === "folder" && n.children.some((c) => c.id === id)) {
        parent = n.id;
        return true;
      }
    });
    return parent;
  };
  const endpointOf = (d: ConnectionDraft) =>
    d.kind === "serial" && d.serial ? `${d.serial.port} ${d.serial.baud}` : `${d.host.trim()}:${d.port}`;

  return {
    appVersion: async () => "0.1.0-dev",
    getTree: async () => structuredClone(tree),
    getDevices: async () => devices,
    getSyncState: async () => ({ state: "synced", transport: "relay", at: now() - 3000 }),
    async ackChange(id) {
      const hit = find(tree, id);
      if (hit?.node.change?.kind === "deleted") hit.siblings.splice(hit.siblings.indexOf(hit.node), 1);
      else clearChange(id);
      emit();
    },
    async ackAll() {
      const doomed: string[] = [];
      walk(tree, (n) => {
        if (n.change?.kind === "deleted") doomed.push(n.id);
        delete n.change;
      });
      doomed.forEach((id) => {
        const hit = find(tree, id);
        hit?.siblings.splice(hit.siblings.indexOf(hit.node), 1);
      });
      emit();
    },
    async revert(opId) {
      walk(tree, (n, siblings) => {
        const entry = n.change?.entries.find((e) => e.opId === opId);
        if (!entry) return;
        if (entry.kind === "added") siblings.splice(siblings.indexOf(n), 1);
        if (entry.kind === "edited" && n.type === "connection") {
          const host = entry.fields.find((f) => f.field === "host");
          if (host?.before) n.endpoint = n.endpoint.replace(host.after ?? "", host.before);
        }
        delete n.change;
        return true;
      });
      emit();
    },
    async moveNode(id, parentId, index) {
      const hit = find(tree, id);
      if (!hit) return;
      hit.siblings.splice(hit.siblings.indexOf(hit.node), 1);
      const target = parentId ? find(tree, parentId) : null;
      const list = target?.node.type === "folder" ? target.node.children : tree;
      list.splice(index, 0, hit.node);
      emit();
    },
    async createFolder(parentId, name) {
      const id = newId();
      childrenOf(parentId).push({ type: "folder", id, name: name.trim(), children: [] });
      emit();
      return id;
    },
    async getConnection(id) {
      const hit = find(tree, id);
      if (hit?.node.type !== "connection") throw new Error("connection not found");
      const n = hit.node;
      const x = extra.get(id);
      const [host, port] =
        n.kind === "serial" ? ["", 0] : [n.endpoint.replace(/:\d+$/, ""), Number(n.endpoint.split(":").pop())];
      const [com, baud] = n.endpoint.split(" ");
      return {
        id,
        folderId: parentOf(id),
        name: n.name,
        kind: n.kind,
        host,
        port,
        serial:
          x?.serial ??
          (n.kind === "serial"
            ? { port: com, baud: Number(baud) || 9600, dataBits: 8, parity: "none", stopBits: 1, flow: "none" }
            : null),
        username: x?.username ?? null,
        tabColor: n.tabColor ?? null,
        jumpHost: x?.jumpHost ?? null,
        log: x?.log ?? null,
        notes: x?.notes ?? "",
      };
    },
    async saveConnection(d) {
      if (!d.name.trim()) throw new Error("Name is required");
      if (d.kind !== "serial" && !d.host.trim()) throw new Error("Host is required");
      const fields = {
        name: d.name.trim(),
        kind: d.kind,
        endpoint: endpointOf(d),
        tabColor: d.tabColor?.trim() || undefined,
      };
      let id = d.id;
      const hit = id ? find(tree, id) : null;
      if (hit?.node.type === "connection") Object.assign(hit.node, fields);
      else {
        id = newId();
        childrenOf(d.folderId).push({ type: "connection", id, ...fields });
      }
      extra.set(id!, {
        username: d.username?.trim() || null,
        notes: d.notes,
        serial: d.serial,
        jumpHost: d.jumpHost,
        log: d.log,
      });
      emit();
      return id!;
    },
    async renameNode(id, name) {
      const hit = find(tree, id);
      if (hit && name.trim()) hit.node.name = name.trim();
      emit();
    },
    async deleteNode(id) {
      const hit = find(tree, id);
      if (hit) hit.siblings.splice(hit.siblings.indexOf(hit.node), 1);
      emit();
    },
    async duplicateConnection(id) {
      const hit = find(tree, id);
      if (hit?.node.type !== "connection") throw new Error("connection not found");
      const copy = { ...structuredClone(hit.node), id: newId(), name: `${hit.node.name} (copy)` };
      delete copy.change;
      hit.siblings.splice(hit.siblings.indexOf(hit.node) + 1, 0, copy);
      const x = extra.get(id);
      if (x) extra.set(copy.id, structuredClone(x));
      emit();
      return copy.id;
    },
    async getPreferences() {
      try {
        return JSON.parse(localStorage.getItem("linecard.prefs") ?? "null");
      } catch {
        return null;
      }
    },
    async setPreferences(p) {
      try {
        localStorage.setItem("linecard.prefs", JSON.stringify(p));
      } catch {
        /* storage unavailable: keep in memory only */
      }
    },
    async listSavedPasswords() {
      return [...savedPasswords].map(([label, savedAt]) => ({ key: `ssh:${label}`, label, savedAt }));
    },
    async forgetSavedPassword(key) {
      savedPasswords.delete(key.replace(/^ssh:/, ""));
    },
    async importSources() {
      return [
        {
          id: "securecrt",
          label: "SecureCRT",
          path: "C:\\Users\\you\\AppData\\Roaming\\VanDyke\\Config\\Sessions",
          found: true,
          hint: "The Config\Sessions folder",
        },
        { id: "putty", label: "PuTTY", path: null, found: false, hint: "Saved sessions are read from the registry" },
        { id: "mremoteng", label: "mRemoteNG", path: null, found: false, hint: "confCons.xml (unencrypted)" },
        {
          id: "ssh_config",
          label: "OpenSSH config",
          path: "C:\\Users\\you\\.ssh\\config",
          found: true,
          hint: "Usually ~/.ssh/config",
        },
      ];
    },
    async importPreview(source) {
      if (source === "putty") throw new Error("no PuTTY sessions found");
      return {
        summary: "3 sessions in 2 folders, 1 warning",
        warnings: ["Skipped 'modem': TAPI sessions are not supported"],
        rows: [
          { folder: "DC2", name: "core1", kind: "ssh", endpoint: "10.30.0.1:22", username: "netadmin" },
          { folder: "DC2 / Edge", name: "edge1", kind: "ssh", endpoint: "10.30.9.1:22", username: null },
          { folder: "", name: "cs2", kind: "telnet", endpoint: "10.30.99.5:2003", username: null },
        ],
      };
    },
    async importApply(source) {
      const id = newId();
      const conn = (name: string, endpoint: string): ConnectionNode => ({
        type: "connection",
        id: newId(),
        name,
        kind: "ssh",
        endpoint,
      });
      tree.push({
        type: "folder",
        id,
        name: source === "ssh_config" ? "Imported from ssh_config" : "Imported from SecureCRT",
        children: [{ type: "folder", id: newId(), name: "DC2", children: [conn("core1", "10.30.0.1:22")] }],
      });
      emit();
      return id;
    },
    async getLogging() {
      return { ...logging, defaultFolder: "C:\\Users\\you\\Documents\\Linecard Logs" };
    },
    async setLogging(l) {
      logging = { enabled: l.enabled, folder: l.folder, timestamps: l.timestamps };
    },
    async listSerialPorts() {
      return [
        { name: "COM3", description: "USB Serial Port" },
        { name: "COM4", description: "Prolific USB-to-Serial" },
      ];
    },
    async clipboardRead() {
      try {
        return await navigator.clipboard.readText();
      } catch {
        return memClipboard;
      }
    },
    async clipboardWrite(text) {
      memClipboard = text;
      await navigator.clipboard.writeText(text).catch(() => {});
    },
    async openSession(node, _size, onEvent): Promise<SessionHandle> {
      const send = (s: string) => onEvent({ event: "data", data: Array.from(new TextEncoder().encode(s)) });
      const host = node.name;
      const prompt = `${host.split(" ")[0]}# `;
      let line = "";
      const connect = () => {
        onEvent({ event: "connected" });
        send(
          `\x1b[2mDemo session (browser preview). Real ${node.kind.toUpperCase()} sessions run in the desktop app.\x1b[0m\r\n\r\n`,
        );
        send(prompt);
      };
      // SSH walks through the same prompts as the real driver: host key on
      // first contact, then a password ("wrong" is refused).
      const [hostname] = node.endpoint.split(":");
      const login = `admin@${hostname}`;
      const askPassword = () =>
        onEvent({
          event: "auth",
          title: `Password for ${login}`,
          instructions: "",
          fields: [{ label: "Password", echo: false }],
          remember: savedPasswords.has(login),
        });
      // A saved password logs straight in, as the real driver does.
      const authenticate = () => (savedPasswords.has(login) ? connect() : askPassword());
      if (node.kind === "ssh") {
        setTimeout(() => {
          if (trustedHosts.has(node.endpoint)) return authenticate();
          onEvent({
            event: "hostKey",
            host: hostname,
            port: Number(node.endpoint.split(":")[1] ?? 22),
            algorithm: "ssh-ed25519",
            fingerprint: "SHA256:iVxAQiFJZeZYlU9KKfVhxXsCjnnmVyysNOGExA17hCU",
            changed: node.id === "spine02" ? { path: "C:\\Users\\you\\.ssh\\known_hosts", line: 14 } : null,
          });
        }, 300);
      } else setTimeout(connect, 250);
      let closed = false;
      return {
        reply(r) {
          if (closed) return;
          if (r.type === "cancel" || (r.type === "hostKey" && r.decision === "reject")) {
            closed = true;
            onEvent({ event: "disconnected", reason: r.type === "cancel" ? "canceled" : "host key not accepted" });
          } else if (r.type === "hostKey") {
            if (r.decision === "acceptAndSave") trustedHosts.add(node.endpoint);
            setTimeout(authenticate, 200);
          } else if (r.values[0] === "wrong" || !r.values[0]) {
            send("\x1b[31mAccess denied\x1b[0m\r\n");
            setTimeout(askPassword, 200);
          } else {
            if (r.remember) savedPasswords.set(login, Date.now());
            setTimeout(connect, 200);
          }
        },
        write(data) {
          for (const ch of data) {
            if (ch === "\r") {
              const cmd = line.trim();
              line = "";
              send("\r\n");
              if (cmd === "exit") {
                onEvent({ event: "disconnected", reason: null });
                return;
              }
              if (cmd === "show version") send("Linecard demo OS, Version 0.1.0\r\nUptime is 42 days\r\n");
              else if (cmd === "show int status") {
                send(
                  "Port      Status       Vlan\r\nEth1/1    \x1b[32mconnected\x1b[0m    10\r\nEth1/2    \x1b[31mdown\x1b[0m         10\r\nEth1/3    \x1b[33merr-disabled\x1b[0m 20\r\n",
                );
              } else if (cmd) send(`% Invalid input: ${cmd}\r\n`);
              send(prompt);
            } else if (ch === "\x7f") {
              if (line) {
                line = line.slice(0, -1);
                send("\b \b");
              }
            } else if (ch >= " ") {
              line += ch;
              send(ch);
            }
          }
        },
        resize() {},
        sendBreak() {
          send("\r\n\x1b[2m[BREAK]\x1b[0m\r\nrommon 1 > ");
        },
        close() {},
      };
    },
    onTreeChanged(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
}
