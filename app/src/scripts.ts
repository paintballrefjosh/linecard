import { useSyncExternalStore } from "react";
import { getBackend, type ScriptRun } from "./api";

export interface ScriptLine {
  stderr: boolean;
  text: string;
}

/** One run of a script (several can be going at once). */
export interface ScriptRunState {
  key: number;
  path: string;
  name: string;
  startedAt: number;
  lines: ScriptLine[];
  running: boolean;
  /** A question waiting for the user. */
  ask: { id: number; prompt: string; secret: boolean } | null;
  /** How it ended, once it has. */
  outcome: { text: string; ok: boolean } | null;
  /** Started by a schedule: nobody is there to answer questions. */
  scheduled: boolean;
}

export interface ScriptsState {
  runs: ScriptRunState[];
  /** The run whose output is shown. */
  selected: number | null;
  /** Runs going right now. */
  running: number;
  /** The first question waiting for the user, and whose it is. */
  ask: { key: number; id: number; prompt: string; secret: boolean } | null;
}

/** Runs kept in the list (finished ones are dropped oldest first). */
const KEEP = 12;
/** Scripts going at once. */
export const MAX_RUNNING = 6;
const MAX_LINES = 5000;

let runs: ScriptRunState[] = [];
let selected: number | null = null;
const handles = new Map<number, ScriptRun>();
let nextKey = 0;
let state = derive();
const listeners = new Set<() => void>();

function derive(): ScriptsState {
  const asking = runs.find((r) => r.ask && !r.scheduled);
  return {
    runs,
    selected,
    running: runs.filter((r) => r.running).length,
    ask: asking?.ask ? { key: asking.key, ...asking.ask } : null,
  };
}

const publish = () => {
  state = derive();
  listeners.forEach((l) => l());
};

const patch = (key: number, p: Partial<ScriptRunState> | ((r: ScriptRunState) => Partial<ScriptRunState>)) => {
  runs = runs.map((r) => (r.key === key ? { ...r, ...(typeof p === "function" ? p(r) : p) } : r));
  publish();
};

const baseName = (path: string) => (path.split(/[\\/]/).pop() ?? path).replace(/\.py$/i, "");

/** Python scripts, several at a time, kept here so closing the window does not lose them. */
export const scripts = {
  get state() {
    return state;
  },
  /** Whether another script may start now. */
  canStart: () => state.running < MAX_RUNNING,
  /** Whether this script has a run going. */
  isRunning: (path: string) => runs.some((r) => r.running && r.path === path),

  async start(path: string, opts: { scheduled?: boolean } = {}): Promise<number | null> {
    if (!scripts.canStart()) return null;
    const key = ++nextKey;
    const scheduled = !!opts.scheduled;
    runs = [
      ...runs,
      {
        key,
        path,
        name: baseName(path),
        startedAt: Date.now(),
        lines: [],
        running: true,
        ask: null,
        outcome: null,
        scheduled,
      },
    ];
    // Keep the list short: drop the oldest finished runs.
    const finished = runs.filter((r) => !r.running);
    if (runs.length > KEEP) {
      const drop = new Set(finished.slice(0, runs.length - KEEP).map((r) => r.key));
      runs = runs.filter((r) => !drop.has(r.key));
    }
    if (!scheduled || selected === null) selected = key;
    publish();
    try {
      const h = await (
        await getBackend()
      ).runScript(path, (e) => {
        if (e.event === "output") {
          patch(key, (r) => {
            const lines = r.lines.length >= MAX_LINES ? r.lines.slice(-MAX_LINES / 2) : r.lines;
            return { lines: [...lines, { stderr: e.stderr, text: e.text }] };
          });
        } else if (e.event === "ask") {
          // Nobody watches a scheduled run: it gets "cancelled" as the answer.
          if (scheduled) handles.get(key)?.answer(e.id, null);
          else patch(key, { ask: { id: e.id, prompt: e.prompt, secret: e.secret } });
        } else {
          handles.delete(key);
          patch(key, {
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
      if (runs.find((r) => r.key === key)?.running) handles.set(key, h);
    } catch (err) {
      patch(key, { running: false, outcome: { text: String(err), ok: false } });
    }
    return key;
  },
  stop(key: number) {
    handles.get(key)?.stop();
  },
  answer(key: number, value: string | null) {
    const r = runs.find((x) => x.key === key);
    if (!r?.ask) return;
    handles.get(key)?.answer(r.ask.id, value);
    patch(key, { ask: null });
  },
  select(key: number) {
    selected = key;
    publish();
  },
  /** Remove finished runs from the list. */
  clear() {
    runs = runs.filter((r) => r.running);
    if (!runs.some((r) => r.key === selected)) selected = runs[0]?.key ?? null;
    publish();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
};

export function useScripts() {
  return useSyncExternalStore(scripts.subscribe, () => state);
}
