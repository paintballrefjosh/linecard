/** Finding connections by typing: the command palette's matching and order. */
import type { ConnectionNode, LocalShell, TreeNode } from "./types.ts";

export interface PaletteItem {
  conn: ConnectionNode;
  /** "DC1 Fort Worth / Spine"; empty at the top level. */
  path: string;
}

/** Most recent first; this many are kept. */
export const RECENT_MAX = 20;

/** Every saved connection with the folders it sits in. */
export function flatten(nodes: TreeNode[], path: string[] = []): PaletteItem[] {
  return nodes.flatMap((n) =>
    n.type === "folder" ? flatten(n.children, [...path, n.name]) : [{ conn: n, path: path.join(" / ") }],
  );
}

/** `id` moved to the front of `recent`, which stays at most RECENT_MAX long. */
export function remember(recent: string[], id: string): string[] {
  return [id, ...recent.filter((r) => r !== id)].slice(0, RECENT_MAX);
}

/**
 * How well `query` fits an item, or null if it doesn't. Every word typed must
 * appear in the name, address or folder path, in order of preference: whole
 * words and starts of names count most, then plain substrings, then letters
 * in order ("sp1" finds "spine01").
 */
export function score(item: PaletteItem, query: string): number | null {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return 0;
  const fields: [string, number][] = [
    [item.conn.name.toLowerCase(), 3],
    [item.conn.endpoint.toLowerCase(), 2],
    [item.path.toLowerCase(), 1],
  ];
  let total = 0;
  for (const w of words) {
    let best: number | null = null;
    for (const [text, weight] of fields) {
      const s = fieldScore(text, w);
      if (s !== null && (best === null || s * weight > best)) best = s * weight;
    }
    if (best === null) return null;
    total += best;
  }
  return total;
}

function fieldScore(text: string, word: string): number | null {
  const at = text.indexOf(word);
  if (at === 0) return text.length === word.length ? 12 : 10;
  if (at > 0) return /[\s./:\-_@]/.test(text[at - 1]) ? 8 : 6;
  // Letters in order, with fewer gaps scoring higher.
  let i = 0;
  let gaps = 0;
  let last = -1;
  for (let j = 0; j < text.length && i < word.length; j++) {
    if (text[j] === word[i]) {
      if (last >= 0 && j !== last + 1) gaps++;
      last = j;
      i++;
    }
  }
  if (i < word.length) return null;
  return Math.max(1, 4 - gaps);
}

/**
 * The palette's list. Nothing typed: recent connections. Otherwise every
 * match, best first; recent use breaks ties, then the name.
 */
export function rank(items: PaletteItem[], query: string, recent: string[], limit = 50): PaletteItem[] {
  const recency = new Map(recent.map((id, i) => [id, recent.length - i]));
  if (!query.trim()) {
    const byId = new Map(items.map((it) => [it.conn.id, it]));
    return recent.flatMap((id) => byId.get(id) ?? []).slice(0, limit);
  }
  return items
    .map((it) => ({ it, s: score(it, query) }))
    .filter((x): x is { it: PaletteItem; s: number } => x.s !== null)
    .sort(
      (a, b) =>
        b.s - a.s ||
        (recency.get(b.it.conn.id) ?? 0) - (recency.get(a.it.conn.id) ?? 0) ||
        a.it.conn.name.localeCompare(b.it.conn.name),
    )
    .slice(0, limit)
    .map((x) => x.it);
}

/** A local shell as something to open in a tab (never saved; see LocalShell). */
export function localConnection(shell: LocalShell): ConnectionNode {
  return { type: "connection", id: `local-${shell.id}`, name: shell.name, kind: "local", endpoint: shell.id };
}

/** Local shells as palette entries. */
export function shellItems(shells: LocalShell[]): PaletteItem[] {
  return shells.map((s) => ({ conn: localConnection(s), path: "Local shell" }));
}
