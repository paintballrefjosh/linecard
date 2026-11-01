import { useEffect, useRef, useState } from "react";
import { FolderOpen, Play, Plus, Square } from "lucide-react";
import { getBackend } from "../api";
import { scripts, useScripts } from "../scripts";
import type { ScriptFile, ScriptStatus } from "../types";
import { relativeTime } from "./ConnectionTree";
import { Modal } from "./Dialog";

/** Run Python scripts that drive the open sessions. */
export function ScriptsDialog({ onClose }: { onClose(): void }) {
  const [status, setStatus] = useState<ScriptStatus | null>(null);
  const [files, setFiles] = useState<ScriptFile[]>([]);
  const [naming, setNaming] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const run = useScripts();
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
  }, [run.lines.length, run.ask]);

  const ready = status?.version != null;
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
  const name = run.path?.split(/[\\/]/).pop();

  return (
    <Modal
      title="Scripts"
      className="modal-scripts"
      onSubmit={onClose}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={() => void runFile()} disabled={!ready || run.running}>
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
            <li key={f.path} className={run.path === f.path ? "is-current" : ""}>
              <span className="script-name">{f.name}</span>
              <span className="script-when">{relativeTime(f.modifiedMs)}</span>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Run ${f.name}`}
                title="Run"
                disabled={!ready || run.running}
                onClick={() => start(f.path)}
              >
                <Play size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="scripts-head">
        <h3>{name ? `Output: ${name}` : "Output"}</h3>
        {run.running ? (
          <button type="button" className="btn btn-danger" onClick={() => scripts.stop()}>
            <Square size={13} /> Stop
          </button>
        ) : (
          run.lines.length > 0 && (
            <button type="button" className="btn" onClick={() => scripts.clear()}>
              Clear
            </button>
          )
        )}
      </div>
      <pre className="script-output" ref={log} tabIndex={0} aria-label="Script output" aria-live="polite">
        {run.lines.length === 0 && !run.running && !run.outcome ? (
          <span className="script-dim">Nothing has run yet.</span>
        ) : (
          run.lines.map((l, i) => (
            <span key={i} className={l.stderr ? "script-err" : undefined}>
              {l.text}
              {"\n"}
            </span>
          ))
        )}
        {run.running && !run.ask && <span className="script-dim">Running…</span>}
        {run.outcome && <span className={run.outcome.ok ? "script-ok" : "script-err"}>{run.outcome.text}</span>}
      </pre>
      {run.ask && <AskForm key={run.ask.id} prompt={run.ask.prompt} secret={run.ask.secret} />}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** A question from the running script. */
function AskForm({ prompt, secret }: { prompt: string; secret: boolean }) {
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
              scripts.answer(value);
            }
          }}
        />
        <button type="button" className="btn btn-primary" onClick={() => scripts.answer(value)}>
          OK
        </button>
        <button type="button" className="btn" onClick={() => scripts.answer(null)}>
          Cancel
        </button>
      </div>
    </div>
  );
}
