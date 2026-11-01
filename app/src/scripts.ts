import { useSyncExternalStore } from "react";
import { getBackend, type ScriptRun } from "./api";

export interface ScriptLine {
  stderr: boolean;
  text: string;
}

export interface ScriptState {
  /** The script last run (or running). */
  path: string | null;
  lines: ScriptLine[];
  running: boolean;
  /** A question waiting for the user. */
  ask: { id: number; prompt: string; secret: boolean } | null;
  /** How it ended, once it has. */
  outcome: { text: string; ok: boolean } | null;
}

const MAX_LINES = 5000;

let state: ScriptState = { path: null, lines: [], running: false, ask: null, outcome: null };
let run: ScriptRun | null = null;
/** Each start gets a number so a stale run's late events are ignored. */
let generation = 0;
const listeners = new Set<() => void>();

const set = (patch: Partial<ScriptState>) => {
  state = { ...state, ...patch };
  listeners.forEach((l) => l());
};

const note = (text: string, stderr = false) => {
  const lines = state.lines.length >= MAX_LINES ? state.lines.slice(-MAX_LINES / 2) : state.lines;
  set({ lines: [...lines, { stderr, text }] });
};

/** The one script run at a time, kept here so closing the window does not lose it. */
export const scripts = {
  get state() {
    return state;
  },
  async start(path: string) {
    if (state.running) return;
    const mine = ++generation;
    set({ path, lines: [], running: true, ask: null, outcome: null });
    try {
      const r = await (
        await getBackend()
      ).runScript(path, (e) => {
        if (mine !== generation) return;
        if (e.event === "output") note(e.text, e.stderr);
        else if (e.event === "ask") set({ ask: { id: e.id, prompt: e.prompt, secret: e.secret } });
        else {
          run = null;
          set({
            running: false,
            ask: null,
            outcome: e.stopped
              ? { text: "Stopped", ok: false }
              : e.code === 0
                ? { text: "Finished", ok: true }
                : { text: `Failed (exit code ${e.code ?? "?"})`, ok: false },
          });
        }
      });
      // The script may have ended before the handle arrived.
      if (mine === generation && state.running) run = r;
    } catch (err) {
      if (mine !== generation) return;
      set({ running: false, outcome: { text: String(err), ok: false } });
    }
  },
  stop() {
    run?.stop();
  },
  answer(value: string | null) {
    if (!state.ask) return;
    run?.answer(state.ask.id, value);
    set({ ask: null });
  },
  clear() {
    if (!state.running) set({ lines: [], outcome: null });
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
};

export function useScripts() {
  return useSyncExternalStore(scripts.subscribe, () => state);
}
