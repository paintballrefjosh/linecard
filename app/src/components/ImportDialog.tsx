import { useEffect, useState } from "react";
import { AlertTriangle, Check } from "lucide-react";
import { getBackend } from "../api";
import type { ImportPreview, ImportSource, ImportSourceId } from "../types";
import { Modal } from "./Dialog";
import { KindIcon } from "./icons";

/** Bring connections in from another client: pick a source, preview what
 *  would be added, then import into a new "Imported from …" folder. */
export function ImportDialog({ onClose, onImported }: { onClose(): void; onImported(folderId: string): void }) {
  const [sources, setSources] = useState<ImportSource[]>([]);
  const [source, setSource] = useState<ImportSourceId | null>(null);
  const [path, setPath] = useState("");
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void getBackend().then((b) =>
      b.importSources().then((s) => {
        setSources(s);
        const first = s.find((x) => x.found) ?? s[0];
        if (first) choose(first);
      }),
    );
  }, []);

  const choose = (s: ImportSource) => {
    setSource(s.id);
    setPath(s.path ?? "");
    setPreview(null);
    setError(null);
  };

  const run = async () => {
    if (!source) return;
    setBusy(true);
    setError(null);
    try {
      const b = await getBackend();
      if (!preview) setPreview(await b.importPreview(source, path || null));
      else {
        onImported(await b.importApply(source, path || null));
        onClose();
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const current = sources.find((s) => s.id === source);
  const count = preview?.rows.length ?? 0;

  return (
    <Modal
      title="Import connections"
      className="modal-import"
      onSubmit={run}
      onClose={onClose}
      actions={
        <>
          {preview && (
            <button type="button" className="btn modal-foot-left" onClick={() => setPreview(null)}>
              Back
            </button>
          )}
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy || !source || (!!preview && count === 0)}>
            {preview ? `Import ${count} connection${count === 1 ? "" : "s"}` : "Preview"}
          </button>
        </>
      }
    >
      {!preview ? (
        <>
          <div className="import-sources" role="radiogroup" aria-label="Import from">
            {sources.map((s) => (
              <button
                key={s.id}
                type="button"
                role="radio"
                aria-checked={source === s.id}
                className="import-source"
                onClick={() => choose(s)}
              >
                <span className="import-source-name">{s.label}</span>
                <span className={s.found ? "import-found" : "import-missing"}>
                  {s.found ? (
                    <>
                      <Check size={12} /> Found
                    </>
                  ) : (
                    "Not found"
                  )}
                </span>
              </button>
            ))}
          </div>
          {current && (
            <div className="import-path">
              <p>{current.hint}</p>
              {current.path !== null && (
                <input
                  className="settings-input"
                  aria-label="File or folder to import"
                  value={path}
                  spellCheck={false}
                  onChange={(e) => setPath(e.target.value)}
                />
              )}
            </div>
          )}
        </>
      ) : (
        <>
          <p className="import-summary">
            <b>{preview.summary}</b>, added to a new folder you can review, move or delete.
          </p>
          {preview.warnings.length > 0 && (
            <details className="import-warnings">
              <summary>
                <AlertTriangle size={14} /> {preview.warnings.length} skipped or changed
              </summary>
              <ul>
                {preview.warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            </details>
          )}
          <div className="import-table-wrap">
            <table className="import-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Folder</th>
                  <th>Address</th>
                  <th>User</th>
                </tr>
              </thead>
              <tbody>
                {preview.rows.slice(0, 500).map((r, i) => (
                  <tr key={i}>
                    <td>
                      <KindIcon kind={r.kind} size={14} /> {r.name}
                    </td>
                    <td>{r.folder}</td>
                    <td className="mono">{r.endpoint}</td>
                    <td>{r.username ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
