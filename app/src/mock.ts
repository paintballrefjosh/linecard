// In-browser stand-in for the Rust backend, used by `npm run dev`.
// It demonstrates every highlight state and simulates a remote change.

import type { Backend, SessionHandle } from "./api";
import type { ConnectionNode, Device, FolderNode, PendingChange, TreeNode } from "./types";

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
      conn("lab-jump", "jump-win", "rdp", "lab-jump.corp:3389"),
      conn("lab-kvm", "kvm-switch", "vnc", "10.20.0.50:5900", {
        change: change("moved", "Laptop-Win11", 26, [{ field: "folder", before: "/DC1 Fort Worth", after: "/Lab" }]),
      }),
      conn("lab-r1", "r1", "telnet", "10.20.0.11:23"),
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
    async openSession(node, _size, onEvent): Promise<SessionHandle> {
      const send = (s: string) => onEvent({ event: "data", data: Array.from(new TextEncoder().encode(s)) });
      const host = node.name;
      const prompt = `${host.split(" ")[0]}# `;
      let line = "";
      setTimeout(() => {
        onEvent({ event: "connected" });
        send(
          `\x1b[2mDemo session (browser preview). Real ${node.kind.toUpperCase()} sessions run in the desktop app.\x1b[0m\r\n\r\n`,
        );
        send(prompt);
      }, 250);
      return {
        write(data) {
          for (const ch of data) {
            if (ch === "\r") {
              const cmd = line.trim();
              line = "";
              send("\r\n");
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
        close() {},
      };
    },
    onTreeChanged(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
}
