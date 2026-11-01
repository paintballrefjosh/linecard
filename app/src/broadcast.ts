import { useSyncExternalStore } from "react";

/** Broadcast input: while on, what you type in a member terminal goes to every
 *  member's session. Terminals register a sender under their tab's id. */
const senders = new Map<string, (data: string) => void>();
let members: ReadonlySet<string> = new Set();
let on = false;
const listeners = new Set<() => void>();
let snapshot = { on, members };

const emit = () => {
  snapshot = { on, members };
  listeners.forEach((l) => l());
};

export const broadcast = {
  /** A terminal offers a way to write to its session (null when it ends). */
  register(id: string, send: ((data: string) => void) | null) {
    if (send) senders.set(id, send);
    else senders.delete(id);
  },
  /** Start broadcasting to these tabs (typically all open terminals). */
  start(ids: string[]) {
    on = true;
    members = new Set(ids);
    emit();
  },
  stop() {
    on = false;
    members = new Set();
    emit();
  },
  toggleMember(id: string) {
    const next = new Set(members);
    if (!next.delete(id)) next.add(id);
    members = next;
    emit();
  },
  /** Forget tabs that were closed. */
  prune(open: string[]) {
    if (!members.size) return;
    const keep = new Set(open);
    const next = new Set([...members].filter((m) => keep.has(m)));
    if (next.size === members.size) return;
    members = next;
    emit();
  },
  /** Input typed in `from`: true if it was fanned out to the group. */
  send(from: string, data: string): boolean {
    if (!on || !members.has(from)) return false;
    for (const id of members) senders.get(id)?.(data);
    return true;
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
};

export function useBroadcast() {
  return useSyncExternalStore(broadcast.subscribe, () => snapshot);
}
