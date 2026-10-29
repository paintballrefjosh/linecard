import type { IDecoration, IDisposable, IMarker, Terminal } from "@xterm/xterm";
import type { Theme } from "./theme";
import { terminalThemes } from "./theme";

/** Named colors follow the terminal theme; anything else is "#rrggbb". */
export type PaletteColor = "red" | "amber" | "green" | "blue" | "purple";

export interface HighlightRule {
  /** A whole word, or a regular expression when `regex` is set. */
  text: string;
  color: PaletteColor | string;
  regex: boolean;
}

/** A named rule set ("Cisco", "Linux"). Profiles sync; folders and
 *  connections pick one, inheriting like the tab color. */
export interface HighlightProfile {
  id: string;
  name: string;
  matchCase: boolean;
  /** Earlier rules win where matches overlap. */
  rules: HighlightRule[];
}

export const palette: { value: PaletteColor; label: string }[] = [
  { value: "red", label: "Red" },
  { value: "amber", label: "Amber" },
  { value: "green", label: "Green" },
  { value: "blue", label: "Blue" },
  { value: "purple", label: "Purple" },
];

/** Network-gear defaults: problems red, attention amber, healthy green.
 *  Keep in step with `default_rules` in src-tauri/src/highlight.rs. */
export const defaultRules: HighlightRule[] = (
  [
    ["err-disabled", "red"],
    ["down", "red"],
    ["error", "red"],
    ["errors", "red"],
    ["failed", "red"],
    ["denied", "red"],
    ["notconnect", "amber"],
    ["warning", "amber"],
    ["flapping", "amber"],
    ["up", "green"],
    ["connected", "green"],
    ["established", "green"],
  ] as const
).map(([text, color]) => ({ text, color, regex: false }));

const ansiName: Record<PaletteColor, keyof (typeof terminalThemes)["dark"]> = {
  red: "red",
  amber: "yellow",
  green: "green",
  blue: "blue",
  purple: "magenta",
};

export const isPalette = (c: string): c is PaletteColor => c in ansiName;

export function colorFor(color: string, theme: Theme): string {
  return isPalette(color) ? terminalThemes[theme][ansiName[color]] : color;
}

/** The regex for one rule; null if it doesn't compile. Words match whole
 *  (hyphens count as part of a word, so "down" doesn't match inside
 *  "err-disabled-down"). */
export function ruleRegex(rule: HighlightRule, matchCase: boolean): RegExp | null {
  const text = rule.regex ? rule.text : rule.text.trim();
  if (!text) return null;
  const source = rule.regex ? text : `(?<![\\w-])${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w-])`;
  try {
    return new RegExp(source, matchCase ? "g" : "gi");
  } catch {
    return null;
  }
}

export type Compiled = { re: RegExp; color: string }[];

export function compileProfile(p: HighlightProfile): Compiled {
  return p.rules.flatMap((r) => {
    const re = ruleRegex(r, p.matchCase);
    return re ? [{ re, color: r.color }] : [];
  });
}

/** Matches in one line of text: [start, length, color]. Earlier rules win
 *  where matches overlap; empty matches are ignored. */
export function findMatches(line: string, compiled: Compiled): [number, number, string][] {
  const out: [number, number, string][] = [];
  const taken = new Uint8Array(line.length);
  for (const { re, color } of compiled) {
    for (const m of line.matchAll(re)) {
      const start = m.index ?? 0;
      const end = start + m[0].length;
      if (end === start) continue;
      let free = true;
      for (let i = start; i < end && free; i++) free = !taken[i];
      if (!free) continue;
      taken.fill(1, start, end);
      out.push([start, end - start, color]);
    }
  }
  return out.sort((a, b) => a[0] - b[0]);
}

/** Profiles as last loaded from the backend (refreshed with the tree). */
let profiles: { profiles: HighlightProfile[]; defaultId: string | null } = { profiles: [], defaultId: null };
const listeners = new Set<() => void>();

export const highlightStore = {
  get: () => profiles,
  set(next: typeof profiles) {
    profiles = next;
    listeners.forEach((l) => l());
  },
  profile: (id: string | null) => profiles.profiles.find((p) => p.id === id) ?? null,
  subscribe(l: () => void) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
};

/** Colors keywords in new output as it's parsed. Only lines that arrived
 *  since the last pass are scanned, plus the line the cursor is on (it may
 *  still be growing); full-screen apps (alternate buffer) are left alone. */
export function attachHighlighter(
  t: Terminal,
  settings: () => { enabled: boolean; profile: HighlightProfile | null; theme: Theme },
): IDisposable {
  let cursorMarker: IMarker | undefined;
  let cursorDecos: IDecoration[] = [];
  let compiledFor: HighlightProfile | null = null;
  let compiled: Compiled = [];

  const sub = t.onWriteParsed(() => {
    const s = settings();
    const buf = t.buffer.active;
    if (!s.enabled || !s.profile || buf.type !== "normal") return;
    if (compiledFor !== s.profile) {
      compiled = compileProfile(s.profile);
      compiledFor = s.profile;
    }
    if (!compiled.length) return;

    const cursorAbs = buf.baseY + buf.cursorY;
    const start = cursorMarker && !cursorMarker.isDisposed ? cursorMarker.line : Math.max(0, cursorAbs - t.rows + 1);
    cursorDecos.forEach((d) => d.dispose());
    cursorDecos = [];
    for (let y = start; y <= cursorAbs; y++) {
      const text = buf.getLine(y)?.translateToString(true) ?? "";
      for (const [x, width, color] of findMatches(text, compiled)) {
        const marker = t.registerMarker(y - cursorAbs);
        if (!marker) continue;
        const deco = t.registerDecoration({ marker, x, width, foregroundColor: colorFor(color, s.theme) });
        if (!deco) continue;
        deco.onDispose(() => marker.dispose());
        if (y === cursorAbs) cursorDecos.push(deco);
      }
    }
    cursorMarker?.dispose();
    cursorMarker = t.registerMarker(0);
  });

  return {
    dispose() {
      sub.dispose();
      cursorMarker?.dispose();
    },
  };
}
