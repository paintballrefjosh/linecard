import type { IDecoration, IDisposable, IMarker, Terminal } from "@xterm/xterm";
import type { Theme } from "./theme";
import { terminalThemes } from "./theme";

export type HighlightColor = "red" | "amber" | "green" | "blue" | "purple";

export interface HighlightRule {
  /** Matched as a whole word, case-insensitively. */
  text: string;
  color: HighlightColor;
}

/** Network-gear defaults: problems red, attention amber, healthy green. */
export const defaultRules: HighlightRule[] = [
  { text: "err-disabled", color: "red" },
  { text: "down", color: "red" },
  { text: "error", color: "red" },
  { text: "errors", color: "red" },
  { text: "failed", color: "red" },
  { text: "denied", color: "red" },
  { text: "notconnect", color: "amber" },
  { text: "warning", color: "amber" },
  { text: "flapping", color: "amber" },
  { text: "up", color: "green" },
  { text: "connected", color: "green" },
  { text: "established", color: "green" },
];

const ansiName: Record<HighlightColor, keyof (typeof terminalThemes)["dark"]> = {
  red: "red",
  amber: "yellow",
  green: "green",
  blue: "blue",
  purple: "magenta",
};

export function colorFor(color: HighlightColor, theme: Theme): string {
  return terminalThemes[theme][ansiName[color]];
}

/** One regex for all rules: whole words (hyphens count as part of a word,
 *  so "down" doesn't match inside "err-disabled-down"), longest first. */
export function compileRules(rules: HighlightRule[]): { re: RegExp; colors: Map<string, HighlightColor> } | null {
  const usable = rules.filter((r) => r.text.trim());
  if (!usable.length) return null;
  const colors = new Map(usable.map((r) => [r.text.trim().toLowerCase(), r.color]));
  const alts = [...colors.keys()]
    .sort((a, b) => b.length - a.length)
    .map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return { re: new RegExp(`(?<![\\w-])(${alts.join("|")})(?![\\w-])`, "gi"), colors };
}

/** Matches in one line of text: [start, length, color]. */
export function findMatches(
  line: string,
  compiled: NonNullable<ReturnType<typeof compileRules>>,
): [number, number, HighlightColor][] {
  const out: [number, number, HighlightColor][] = [];
  for (const m of line.matchAll(compiled.re)) {
    const color = compiled.colors.get(m[1].toLowerCase());
    if (color && m.index !== undefined) out.push([m.index, m[1].length, color]);
  }
  return out;
}

/** Colors keywords in new output as it's parsed. Only lines that arrived
 *  since the last pass are scanned, plus the line the cursor is on (it may
 *  still be growing); full-screen apps (alternate buffer) are left alone. */
export function attachHighlighter(
  t: Terminal,
  settings: () => { enabled: boolean; rules: HighlightRule[]; theme: Theme },
): IDisposable {
  let cursorMarker: IMarker | undefined;
  let cursorDecos: IDecoration[] = [];
  let compiledFor: HighlightRule[] | null = null;
  let compiled: ReturnType<typeof compileRules> = null;

  const sub = t.onWriteParsed(() => {
    const s = settings();
    const buf = t.buffer.active;
    if (!s.enabled || buf.type !== "normal") return;
    if (compiledFor !== s.rules) {
      compiled = compileRules(s.rules);
      compiledFor = s.rules;
    }
    if (!compiled) return;

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
