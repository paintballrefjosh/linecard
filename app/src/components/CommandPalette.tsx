import { useEffect, useMemo, useRef, useState } from "react";
import { Clock, Search, Zap } from "lucide-react";
import type { ConnectionNode, TreeNode } from "../types";
import { flatten, rank, type PaletteItem } from "../palette";
import { KindIcon } from "./icons";

interface Props {
  tree: TreeNode[];
  /** Saved connection ids and quick connects ("quick-…"), newest first. */
  recent: string[];
  /** Opened like connections, but not in the tree (local shells). */
  extra?: PaletteItem[];
  /** Reads typed text as an ad-hoc connection, if it is one. */
  parseQuick(text: string): ConnectionNode | null;
  onOpen(conn: ConnectionNode): void;
  onClose(): void;
}

type Row = { conn: ConnectionNode; detail: string; quick?: boolean; recent?: boolean };

/** Ctrl+Shift+P: find a connection by typing and open it. */
export function CommandPalette({ tree, recent, extra, parseQuick, onOpen, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [at, setAt] = useState(0);
  const list = useRef<HTMLUListElement>(null);
  const items = useMemo(() => [...flatten(tree), ...(extra ?? [])], [tree, extra]);

  const rows: Row[] = useMemo(() => {
    const out: Row[] = [];
    const quick = query.trim() ? parseQuick(query) : null;
    // Something typed that isn't a saved name: offer to connect to it as is.
    // Any word reads as a host name, so it goes first only when nothing saved matches.
    const found = rank(items, query, recent);
    out.push(...found.map((f) => ({ conn: f.conn, detail: f.path ? `${f.path} · ${f.conn.endpoint}` : f.conn.endpoint })));
    if (quick && !found.some((f) => f.conn.endpoint === quick.endpoint))
      out.push({ conn: quick, detail: `${quick.kind.toUpperCase()} ${quick.endpoint}`, quick: true });
    if (!query.trim()) {
      // Recent quick connects aren't in the tree; rebuild them from their text.
      const recentQuick = recent
        .filter((id) => id.startsWith("quick-"))
        .flatMap((id) => parseQuick(id.slice("quick-".length)) ?? []);
      const order = new Map(recent.map((id, i) => [id, i]));
      return [...out, ...recentQuick.map((c) => ({ conn: c, detail: `${c.kind.toUpperCase()} ${c.endpoint}`, quick: true }))]
        .sort((a, b) => (order.get(a.conn.id) ?? 0) - (order.get(b.conn.id) ?? 0))
        .map((r) => ({ ...r, recent: true }));
    }
    return out;
  }, [items, query, recent, parseQuick]);

  useEffect(() => setAt(0), [query]);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at]);

  const choose = (r: Row | undefined) => {
    if (!r) return;
    onClose();
    onOpen(r.conn);
  };

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="Open a connection"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="palette-search">
          <Search size={16} aria-hidden />
          <input
            autoFocus
            value={query}
            placeholder="Connect to… (name, address, folder, or ssh admin@host)"
            aria-label="Connection to open"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={rows.length ? `palette-row-${at}` : undefined}
            spellCheck={false}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setAt((i) => Math.min(rows.length - 1, i + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setAt((i) => Math.max(0, i - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                choose(rows[at]);
              } else if (e.key === "Escape") {
                e.preventDefault();
                onClose();
              }
            }}
          />
        </div>
        {rows.length > 0 && !query.trim() && <div className="palette-heading">Recent</div>}
        <ul className="palette-list" id="palette-list" role="listbox" ref={list}>
          {rows.map((r, i) => (
            <li
              key={`${r.conn.id}-${i}`}
              id={`palette-row-${i}`}
              data-index={i}
              role="option"
              aria-selected={i === at}
              className={i === at ? "is-active" : ""}
              onMouseMove={() => setAt(i)}
              onClick={() => choose(r)}
            >
              <span className="palette-icon">{r.quick ? <Zap size={15} /> : <KindIcon kind={r.conn.kind} />}</span>
              <span className="palette-name">{r.quick && query.trim() ? `Connect to ${r.conn.name}` : r.conn.name}</span>
              <span className="palette-detail">{r.detail}</span>
              {r.recent && <Clock size={13} className="palette-recent" aria-label="Recent" />}
            </li>
          ))}
          {rows.length === 0 && (
            <li className="palette-empty" role="presentation">
              {query.trim() ? "No connections match" : "Connections you open show up here"}
            </li>
          )}
        </ul>
      </div>
    </div>
  );
}
