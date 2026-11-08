import { useEffect, useState } from "react";
import { ArrowDown, ArrowUp, Trash2 } from "lucide-react";
import { getBackend } from "../api";
import type { Snippet, TreeNode } from "../types";
import { Modal } from "./Dialog";

/** Every folder as (id, "/path"), for choosing where a button shows. */
function folders(nodes: TreeNode[], prefix = ""): { id: string; path: string }[] {
  return nodes.flatMap((n) =>
    n.type === "folder" ? [{ id: n.id, path: `${prefix}/${n.name}` }, ...folders(n.children, `${prefix}/${n.name}`)] : [],
  );
}

const blank = (): Snippet => ({ id: "", name: "", text: "", folder: null, order: 0, enter: true });

/** Add, edit, reorder and remove the button bar's commands. */
export function SnippetsDialog({ tree, onClose }: { tree: TreeNode[]; onClose(): void }) {
  const [items, setItems] = useState<Snippet[]>([]);
  const [draft, setDraft] = useState<Snippet | null>(null);
  const [error, setError] = useState<string | null>(null);
  const places = folders(tree);
  const where = (s: Snippet) => places.find((p) => p.id === s.folder)?.path ?? "All connections";

  const load = () => void getBackend().then((b) => b.listSnippets().then(setItems));
  useEffect(load, []);

  const run = (f: (b: Awaited<ReturnType<typeof getBackend>>) => Promise<unknown>) =>
    getBackend()
      .then(f)
      .then(() => setError(null))
      .catch((e) => setError(String(e)))
      .finally(load);

  const save = async () => {
    if (!draft) return;
    try {
      await (await getBackend()).saveSnippet(draft);
      setDraft(null);
      setError(null);
      load();
    } catch (e) {
      setError(String(e));
    }
  };
  const move = (i: number, by: number) => {
    const ids = items.map((s) => s.id);
    [ids[i], ids[i + by]] = [ids[i + by], ids[i]];
    void run((b) => b.reorderSnippets(ids));
  };

  if (draft)
    return (
      <Modal
        key="form"
        title={draft.id ? "Edit button" : "New button"}
        onSubmit={save}
        onClose={() => setDraft(null)}
        actions={
          <>
            <button type="button" className="btn" onClick={() => setDraft(null)}>
              Back
            </button>
            <button type="submit" className="btn btn-primary">
              Save
            </button>
          </>
        }
      >
        <div className="form-grid">
          <label htmlFor="sn-name">Label</label>
          <input
            id="sn-name"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            data-autofocus
          />
          <label htmlFor="sn-text">Text to send</label>
          <textarea
            id="sn-text"
            rows={4}
            value={draft.text}
            onChange={(e) => setDraft({ ...draft, text: e.target.value })}
            spellCheck={false}
          />
          <label htmlFor="sn-enter">Press Enter after</label>
          <input
            id="sn-enter"
            style={{ justifySelf: "start", width: "auto" }}
            type="checkbox"
            checked={draft.enter}
            onChange={(e) => setDraft({ ...draft, enter: e.target.checked })}
          />
          <label htmlFor="sn-where">Shows for</label>
          <select
            id="sn-where"
            value={draft.folder ?? ""}
            onChange={(e) => setDraft({ ...draft, folder: e.target.value || null })}
          >
            <option value="">All connections</option>
            {places.map((p) => (
              <option key={p.id} value={p.id}>
                {p.path}
              </option>
            ))}
          </select>
        </div>
        {error && <p className="form-error">{error}</p>}
      </Modal>
    );

  return (
    <Modal
      key="list"
      title="Button bar"
      onSubmit={onClose}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={() => setDraft(blank())}>
            Add button…
          </button>
          <button type="submit" className="btn btn-primary" data-autofocus>
            Done
          </button>
        </>
      }
    >
      {items.length === 0 ? (
        <p>Save commands you type often, then send each with one click.</p>
      ) : (
        <ul className="snippet-list">
          {items.map((s, i) => (
            <li key={s.id}>
              <button type="button" className="snippet-main" onClick={() => setDraft(s)}>
                <b>{s.name}</b>
                <span>{where(s)}</span>
                <code>{s.text.split(/\r?\n/)[0]}</code>
              </button>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Move ${s.name} up`}
                disabled={i === 0}
                onClick={() => move(i, -1)}
              >
                <ArrowUp size={14} />
              </button>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Move ${s.name} down`}
                disabled={i === items.length - 1}
                onClick={() => move(i, 1)}
              >
                <ArrowDown size={14} />
              </button>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Delete ${s.name}`}
                onClick={() => void run((b) => b.deleteSnippet(s.id))}
              >
                <Trash2 size={14} />
              </button>
            </li>
          ))}
        </ul>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
