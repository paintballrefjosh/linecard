import { useEffect, useState } from "react";
import type { DockviewApi } from "dockview-react";
import { Pencil } from "lucide-react";
import { getBackend } from "../api";
import { sendInput } from "../broadcast";
import type { ConnectionNode, Snippet, TreeNode } from "../types";
import { SnippetsDialog } from "./SnippetsDialog";

/** The text to type for a snippet: line breaks become Enter (CR), plus one
 *  more at the end when the snippet asks for it. */
export function snippetInput(s: Pick<Snippet, "text" | "enter">): string {
  const body = s.text.replace(/\r\n?|\n/g, "\r");
  return s.enter && !body.endsWith("\r") ? `${body}\r` : body;
}

interface Active {
  id: string;
  conn: ConnectionNode;
  files: boolean;
}

/** Saved commands for the active terminal tab: one click types the text into
 *  its session (and the whole broadcast group, when broadcasting). */
export function ButtonBar({ dock, tree }: { dock: DockviewApi | null; tree: TreeNode[] }) {
  const [active, setActive] = useState<Active | null>(null);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [managing, setManaging] = useState(false);

  useEffect(() => {
    if (!dock) return;
    const read = () => {
      const p = dock.activePanel;
      const params = p?.params as { conn?: ConnectionNode; files?: boolean } | undefined;
      const conn = params?.conn;
      setActive(p && conn ? { id: p.id, conn, files: !!params?.files } : null);
    };
    read();
    const subs = [dock.onDidActivePanelChange(read), dock.onDidRemovePanel(read)];
    return () => subs.forEach((s) => s.dispose());
  }, [dock]);

  // `tree` changes when the library does (including snippets synced in).
  const connId = active && !active.conn.id.startsWith("quick-") ? active.conn.id : null;
  useEffect(() => {
    let live = true;
    void getBackend()
      .then((b) => b.snippetsFor(connId))
      .then((s) => live && setSnippets(s));
    return () => {
      live = false;
    };
  }, [connId, tree, managing]);

  const terminal = active && !active.files && active.conn.kind !== "rdp" && active.conn.kind !== "vnc";
  return (
    <div className="button-bar" role="toolbar" aria-label="Saved commands">
      {snippets.length === 0 && <span className="button-bar-empty">No saved commands yet.</span>}
      {snippets.map((s) => (
        <button
          key={s.id}
          className="btn button-bar-btn"
          disabled={!terminal}
          title={terminal ? s.text : "Select a terminal tab to send this"}
          onClick={() => active && sendInput(active.id, snippetInput(s))}
        >
          {s.name}
        </button>
      ))}
      <span className="button-bar-spacer" />
      <button className="icon-btn" onClick={() => setManaging(true)} aria-label="Edit buttons" title="Edit buttons">
        <Pencil size={14} />
      </button>
      {managing && <SnippetsDialog tree={tree} onClose={() => setManaging(false)} />}
    </div>
  );
}
