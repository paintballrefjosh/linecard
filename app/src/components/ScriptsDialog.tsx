import { useEffect, useRef, useState } from "react";
import { FolderOpen, Play, Plus, Square, X } from "lucide-react";
import { getBackend } from "../api";
import { MAX_RUNNING, scripts, useScripts } from "../scripts";
import { describe, timeToday, type Schedule } from "../schedule";
import { usePrefs } from "../prefs";
import type { ScriptFile, ScriptStatus, TreeNode } from "../types";
import { relativeTime } from "./ConnectionTree";
import { Modal } from "./Dialog";

/** Run Python scripts that drive the open sessions. */
/** Every connection as (id, "folder/name"), for choosing one. */
function connectionsIn(nodes: TreeNode[], prefix = ""): { id: string; path: string }[] {
  return nodes.flatMap((n) =>
    n.type === "folder"
      ? connectionsIn(n.children, `${prefix}${n.name}/`)
      : n.kind === "rdp" || n.kind === "vnc"
        ? []
        : [{ id: n.id, path: `${prefix}${n.name}` }],
  );
}

export function ScriptsDialog({ tree, onClose }: { tree: TreeNode[]; onClose(): void }) {
  const { prefs, update } = usePrefs();
  const [ruleConn, setRuleConn] = useState("");
  const [ruleScript, setRuleScript] = useState("");
  const [planScript, setPlanScript] = useState("");
  const [planKind, setPlanKind] = useState<"every" | "at">("every");
  const [planEvery, setPlanEvery] = useState("30");
  const [planAt, setPlanAt] = useState("06:00");
  const places = connectionsIn(tree);
  const [status, setStatus] = useState<ScriptStatus | null>(null);
  const [files, setFiles] = useState<ScriptFile[]>([]);
  const [naming, setNaming] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const all = useScripts();
  const run = all.runs.find((r) => r.key === all.selected) ?? null;
  const log = useRef<HTMLPreElement>(null);

  const load = () => void getBackend().then((b) => b.listScripts().then(setFiles));
  useEffect(() => {
    void getBackend().then((b) => b.scriptStatus().then(setStatus));
    load();
  }, []);
  // Keep the newest output in view.
  useEffect(() => {
    const el = log.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [run?.key, run?.lines.length, run?.ask]);

  const ready = status?.version != null;
  const room = scripts.canStart();
  const planOk = planKind === "every" ? Number(planEvery) >= 1 : timeToday(planAt, new Date()) !== null;
  const nameOf = (path: string) => (path.split(/[\\/]/).pop() ?? path).replace(/\.py$/i, "");
  const start = (path: string) => {
    setError(null);
    void scripts.start(path);
  };
  const create = async () => {
    if (!naming?.trim()) return;
    try {
      const b = await getBackend();
      await b.newScript(naming.trim());
      setNaming(null);
      setError(null);
      load();
      // Show the folder so the new file can be opened in an editor.
      await b.openScriptsFolder();
    } catch (e) {
      setError(String(e));
    }
  };
  const runFile = async () => {
    const b = await getBackend();
    const path = await b.pickFile("open", "Python script to run");
    if (path) start(path);
  };
  const name = run?.name;

  return (
    <Modal
      title="Scripts"
      className="modal-scripts"
      onSubmit={onClose}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={() => void runFile()} disabled={!ready || !room}>
            Run a file…
          </button>
          <button type="submit" className="btn btn-primary" data-autofocus>
            Close
          </button>
        </>
      }
    >
      <p className={`scripts-python ${status && !ready ? "has-problem" : ""}`} role="status">
        {!status ? "Looking for Python…" : ready ? `Python ${status.version}` : (status.problem ?? "Python was not found.")}
      </p>

      <div className="scripts-head">
        <h3>Your scripts</h3>
        <button type="button" className="btn" onClick={() => setNaming("")} disabled={naming !== null}>
          <Plus size={14} /> New…
        </button>
        <button type="button" className="btn" onClick={() => void getBackend().then((b) => b.openScriptsFolder())}>
          <FolderOpen size={14} /> Open folder
        </button>
      </div>
      {naming !== null && (
        <div className="scripts-new">
          <input
            aria-label="Script name"
            placeholder="Name, e.g. collect-versions"
            value={naming}
            onChange={(e) => setNaming(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void create();
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setNaming(null);
              }
            }}
            autoFocus
            spellCheck={false}
          />
          <button type="button" className="btn btn-primary" onClick={() => void create()}>
            Create
          </button>
        </div>
      )}
      {files.length === 0 ? (
        <p className="scripts-empty">
          No scripts yet. Create one, or put <code>.py</code> files in the folder. Each starts with{" "}
          <code>import linecard as lc</code>.
        </p>
      ) : (
        <ul className="script-list">
          {files.map((f) => (
            <li key={f.path} className={scripts.isRunning(f.path) ? "is-current" : ""}>
              <span className="script-name">{f.name}</span>
              <span className="script-when">{relativeTime(f.modifiedMs)}</span>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Run ${f.name}`}
                title="Run"
                disabled={!ready || !room}
                onClick={() => start(f.path)}
              >
                <Play size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="scripts-head">
        <h3>Run on connect</h3>
      </div>
      {prefs.onConnect.length > 0 && (
        <ul className="script-list">
          {prefs.onConnect.map((r) => (
            <li key={r.connectionId}>
              <span className="script-name">
                {places.find((p) => p.id === r.connectionId)?.path ?? "(deleted connection)"} →{" "}
                {r.script.split(/[\/]/).pop()}
              </span>
              <button
                type="button"
                className="icon-btn"
                aria-label="Stop running it on connect"
                title="Remove"
                onClick={() => update({ onConnect: prefs.onConnect.filter((x) => x !== r) })}
              >
                <X size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="scripts-new">
        <select aria-label="Connection" value={ruleConn} onChange={(e) => setRuleConn(e.target.value)}>
          <option value="">When this connects…</option>
          {places.map((p) => (
            <option key={p.id} value={p.id}>
              {p.path}
            </option>
          ))}
        </select>
        <select aria-label="Script to run" value={ruleScript} onChange={(e) => setRuleScript(e.target.value)}>
          <option value="">…run this script</option>
          {files.map((f) => (
            <option key={f.path} value={f.path}>
              {f.name}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="btn"
          disabled={!ruleConn || !ruleScript}
          onClick={() => {
            update({
              onConnect: [...prefs.onConnect.filter((r) => r.connectionId !== ruleConn), { connectionId: ruleConn, script: ruleScript }],
            });
            setRuleConn("");
            setRuleScript("");
          }}
        >
          Add
        </button>
      </div>

      <div className="scripts-head">
        <h3>Run on a schedule</h3>
      </div>
      {prefs.schedules.length > 0 && (
        <ul className="script-list">
          {prefs.schedules.map((r) => (
            <li key={r.id}>
              <span className="script-name">
                {nameOf(r.script)} · {describe(r)}
              </span>
              <button
                type="button"
                className="icon-btn"
                aria-label="Stop running it on this schedule"
                title="Remove"
                onClick={() => update({ schedules: prefs.schedules.filter((x) => x.id !== r.id) })}
              >
                <X size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="scripts-new">
        <select aria-label="Script to schedule" value={planScript} onChange={(e) => setPlanScript(e.target.value)}>
          <option value="">Run this script…</option>
          {files.map((f) => (
            <option key={f.path} value={f.path}>
              {f.name}
            </option>
          ))}
        </select>
        <select
          aria-label="How often"
          value={planKind}
          onChange={(e) => setPlanKind(e.target.value as "every" | "at")}
        >
          <option value="every">every… minutes</option>
          <option value="at">daily at…</option>
        </select>
        {planKind === "every" ? (
          <input
            aria-label="Minutes between runs"
            type="number"
            min={1}
            className="port-input"
            value={planEvery}
            onChange={(e) => setPlanEvery(e.target.value)}
          />
        ) : (
          <input
            aria-label="Time of day"
            type="time"
            value={planAt}
            onChange={(e) => setPlanAt(e.target.value)}
          />
        )}
        <button
          type="button"
          className="btn"
          disabled={!planScript || !planOk}
          onClick={() => {
            const rule: Schedule =
              planKind === "every"
                ? { id: `${Date.now()}`, script: planScript, every: Math.floor(Number(planEvery)) }
                : { id: `${Date.now()}`, script: planScript, at: planAt };
            update({ schedules: [...prefs.schedules, rule] });
            setPlanScript("");
          }}
        >
          Add
        </button>
      </div>
      <p className="scripts-note">
        Scheduled scripts run while Linecard is open, one at a time per script, and can&apos;t ask questions. Daily runs
        missed because Linecard was closed are skipped.
      </p>

      <div className="scripts-head">
        <h3>{name ? `Output: ${name}` : "Output"}</h3>
        {run?.running ? (
          <button type="button" className="btn btn-danger" onClick={() => scripts.stop(run.key)}>
            <Square size={13} /> Stop
          </button>
        ) : (
          all.runs.some((r) => !r.running) && (
            <button type="button" className="btn" onClick={() => scripts.clear()}>
              Clear finished
            </button>
          )
        )}
      </div>
      {all.runs.length > 1 && (
        <div className="script-runs" role="tablist" aria-label={`Script runs (up to ${MAX_RUNNING} at once)`}>
          {all.runs.map((r) => (
            <button
              key={r.key}
              type="button"
              role="tab"
              aria-selected={r.key === all.selected}
              className={`script-run ${r.key === all.selected ? "is-selected" : ""}`}
              onClick={() => scripts.select(r.key)}
              title={r.path}
            >
              <span className={`script-dot ${r.running ? "is-running" : r.outcome?.ok ? "is-ok" : "is-bad"}`} />
              {r.name}
              {r.scheduled ? " ⏲" : ""}
            </button>
          ))}
        </div>
      )}
      <pre className="script-output" ref={log} tabIndex={0} aria-label="Script output" aria-live="polite">
        {!run || (run.lines.length === 0 && !run.running && !run.outcome) ? (
          <span className="script-dim">Nothing has run yet.</span>
        ) : (
          run.lines.map((l, i) => (
            <span key={i} className={l.stderr ? "script-err" : undefined}>
              {l.text}
              {"\n"}
            </span>
          ))
        )}
        {run?.running && !run.ask && <span className="script-dim">Running…</span>}
        {run?.outcome && <span className={run.outcome.ok ? "script-ok" : "script-err"}>{run.outcome.text}</span>}
      </pre>
      {all.ask && (
        <AskForm
          key={`${all.ask.key}-${all.ask.id}`}
          runKey={all.ask.key}
          prompt={all.ask.prompt}
          secret={all.ask.secret}
        />
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** A question from the running script. */
function AskForm({ runKey, prompt, secret }: { runKey: number; prompt: string; secret: boolean }) {
  const [value, setValue] = useState("");
  return (
    <div className="script-ask" role="group" aria-label={prompt}>
      <label htmlFor="script-ask-input">{prompt}</label>
      <div>
        <input
          id="script-ask-input"
          type={secret ? "password" : "text"}
          value={value}
          autoFocus
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              scripts.answer(runKey, value);
            }
          }}
        />
        <button type="button" className="btn btn-primary" onClick={() => scripts.answer(runKey, value)}>
          OK
        </button>
        <button type="button" className="btn" onClick={() => scripts.answer(runKey, null)}>
          Cancel
        </button>
      </div>
    </div>
  );
}
