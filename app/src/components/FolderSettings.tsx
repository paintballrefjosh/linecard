import { useEffect, useState } from "react";
import { getBackend } from "../api";
import type { ConnectionNode, FolderSettings, Inherited } from "../types";
import { HighlightSelect, swatches } from "./ConnectionEditor";
import { Modal } from "./Dialog";

interface Props {
  folderId: string;
  /** Where this folder's own inherited values come from. */
  parentId: string | null;
  /** "/DC1 Fort Worth/Spine" */
  path: string;
  jumpHosts: ConnectionNode[];
  onClose(): void;
}

/** Defaults a folder passes down to everything inside it. */
export function FolderSettingsDialog({ folderId, parentId, path, jumpHosts, onClose }: Props) {
  const [s, setS] = useState<FolderSettings | null>(null);
  const [inherited, setInherited] = useState<Inherited | null>(null);
  const [error, setError] = useState<string | null>(null);
  const set = (patch: Partial<FolderSettings>) => setS((cur) => cur && { ...cur, ...patch });

  useEffect(() => {
    void getBackend().then((b) => {
      void b.getFolderSettings(folderId).then(setS);
      void b.inheritedSettings(parentId).then(setInherited);
    });
  }, [folderId, parentId]);

  const save = async () => {
    if (!s) return;
    try {
      await (await getBackend()).saveFolderSettings(folderId, s);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };
  const from = (v: { value: string; from: string } | null | undefined, empty: string) =>
    v ? `Inherited: ${v.value} (from ${v.from})` : empty;

  return (
    <Modal
      title={`Folder settings: ${path}`}
      className="modal-editor"
      onSubmit={save}
      onClose={onClose}
      actions={
        <>
          <span className="modal-foot-note">Folders and connections inside can override these.</span>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={!s}>
            Save
          </button>
        </>
      }
    >
      {s && (
        <div className="form-grid">
          <label htmlFor="fs-user">Username</label>
          <input
            id="fs-user"
            value={s.username ?? ""}
            onChange={(e) => set({ username: e.target.value })}
            placeholder={
              inherited?.username
                ? `${inherited.username.value} (from ${inherited.username.from})`
                : "Asked when connecting"
            }
            spellCheck={false}
            autoComplete="off"
            data-autofocus
          />

          <label htmlFor="fs-jump">SSH jump host</label>
          <select id="fs-jump" value={s.jumpHost ?? ""} onChange={(e) => set({ jumpHost: e.target.value || null })}>
            <option value="">{from(inherited?.jumpHost, "None: connect directly")}</option>
            {jumpHosts.map((j) => (
              <option key={j.id} value={j.id}>
                {j.name} ({j.endpoint})
              </option>
            ))}
          </select>

          <label htmlFor="fs-hl">Keyword highlighting</label>
          <HighlightSelect
            id="fs-hl"
            value={s.highlight}
            inherited={inherited?.highlight ?? null}
            onChange={(highlight) => set({ highlight })}
          />

          <label htmlFor="fs-log">Session log</label>
          <select
            id="fs-log"
            value={s.log === null ? "" : s.log ? "on" : "off"}
            onChange={(e) => set({ log: e.target.value === "" ? null : e.target.value === "on" })}
          >
            <option value="">
              {inherited?.log
                ? `Inherited: ${inherited.log.value === "on" ? "logging" : "not logging"} (from ${inherited.log.from})`
                : "Inherited"}
            </option>
            <option value="on">Log every connection in this folder</option>
            <option value="off">Don't log connections in this folder</option>
          </select>

          <span className="form-label">Tab color</span>
          <div className="swatches" role="radiogroup" aria-label="Tab color">
            <button
              type="button"
              role="radio"
              aria-checked={!s.tabColor}
              className="swatch swatch-none"
              aria-label={inherited?.tabColor ? `Inherited color from ${inherited.tabColor.from}` : "No color"}
              title={from(inherited?.tabColor, "No color")}
              style={inherited?.tabColor ? { background: inherited.tabColor.value } : undefined}
              onClick={() => set({ tabColor: null })}
            />
            {swatches.map((c) => (
              <button
                key={c}
                type="button"
                role="radio"
                aria-checked={s.tabColor === c}
                aria-label={c}
                className="swatch"
                style={{ background: c }}
                onClick={() => set({ tabColor: c })}
              />
            ))}
          </div>
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
