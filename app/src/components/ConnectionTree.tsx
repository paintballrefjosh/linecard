import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Tree, type NodeApi, type NodeRendererProps } from "react-arborist";
import { Check, ChevronDown, ChevronRight, Folder, FolderOpen, Search } from "lucide-react";
import type { ChangeKind, ConnectionNode, PendingChange, TreeNode } from "../types";
import { ChangeGlyph, KindIcon, Undo2, changeLabel } from "./icons";

interface Props {
  tree: TreeNode[];
  onOpen(conn: ConnectionNode): void;
  onAck(id: string): void;
  onAckAll(): void;
  onRevert(opId: string): void;
  onMove(id: string, parentId: string | null, index: number): void;
}

/** Count changed nodes below a folder (for the dot on collapsed folders). */
function changedBelow(node: TreeNode): number {
  if (node.type !== "folder") return 0;
  return node.children.reduce((n, c) => n + (c.change ? 1 : 0) + changedBelow(c), 0);
}

/** Keep only changed nodes and the folders that lead to them. */
function onlyChanged(nodes: TreeNode[]): TreeNode[] {
  return nodes.flatMap((n): TreeNode[] => {
    if (n.type === "folder") {
      const kids = onlyChanged(n.children);
      return n.change || kids.length ? [{ ...n, children: kids }] : [];
    }
    return n.change ? [n] : [];
  });
}

function collectChanges(nodes: TreeNode[], out: ChangeKind[] = []): ChangeKind[] {
  for (const n of nodes) {
    if (n.change) out.push(n.change.kind);
    if (n.type === "folder") collectChanges(n.children, out);
  }
  return out;
}

export function relativeTime(ms: number): string {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} h ago`;
  return `${Math.round(h / 24)} days ago`;
}

function useSize<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setSize({ width: e.contentRect.width, height: e.contentRect.height }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, size] as const;
}

interface HoverState {
  node: TreeNode;
  top: number;
  left: number;
}

const HoverCtx = createContext<{ showHover(node: TreeNode, el: HTMLElement): void; scheduleHide(): void }>({
  showHover: () => {},
  scheduleHide: () => {},
});

function Row({ node, style, dragHandle }: NodeRendererProps<TreeNode>) {
  const { showHover, scheduleHide } = useContext(HoverCtx);
  const d = node.data;
  const change = d.change;
  const below = d.type === "folder" && !node.isOpen ? changedBelow(d) : 0;
  return (
    <div
      ref={dragHandle}
      style={style}
      className={`tree-row ${node.isSelected ? "is-selected" : ""} ${change ? `chg chg-${change.kind}` : ""}`}
      onMouseEnter={change ? (e) => showHover(d, e.currentTarget) : undefined}
      onMouseLeave={change ? scheduleHide : undefined}
      onClick={() => d.type === "folder" && node.toggle()}
    >
      {d.type === "folder" ? (
        <>
          <span className="twisty">{node.isOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
          {node.isOpen ? (
            <FolderOpen size={16} className="folder-icon" />
          ) : (
            <Folder size={16} className="folder-icon" />
          )}
        </>
      ) : (
        <>
          <span className="twisty" />
          <KindIcon kind={d.kind} />
        </>
      )}
      <span className="tree-name">{d.name}</span>
      {d.type === "connection" && <span className="tree-endpoint">{d.endpoint}</span>}
      {change && (
        <span className="chg-glyph" aria-label={changeLabel[change.kind]}>
          <ChangeGlyph kind={change.kind} />
        </span>
      )}
      {below > 0 && (
        <span className="chg-count" title={`${below} changed inside`}>
          {below}
        </span>
      )}
    </div>
  );
}

export function ConnectionTree({ tree, onOpen, onAck, onAckAll, onRevert, onMove }: Props) {
  const [search, setSearch] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const [hover, setHover] = useState<HoverState | null>(null);
  const hideTimer = useRef<number | undefined>(undefined);
  const [boxRef, size] = useSize<HTMLDivElement>();

  const changes = useMemo(() => collectChanges(tree), [tree]);
  const counts = useMemo(() => {
    const c = new Map<ChangeKind, number>();
    changes.forEach((k) => c.set(k, (c.get(k) ?? 0) + 1));
    return c;
  }, [changes]);
  const data = reviewing ? onlyChanged(tree) : tree;

  useEffect(() => {
    if (changes.length === 0) setReviewing(false);
  }, [changes.length]);

  const showHover = (node: TreeNode, el: HTMLElement) => {
    window.clearTimeout(hideTimer.current);
    const r = el.getBoundingClientRect();
    setHover({ node, top: r.top, left: r.right + 8 });
  };
  const scheduleHide = () => {
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => setHover(null), 180);
  };

  const summary = (["added", "edited", "moved", "deleted", "restored", "reverted"] as ChangeKind[])
    .filter((k) => counts.get(k))
    .map((k) => (
      <span key={k} className={`sum sum-${k}`}>
        {counts.get(k)} {changeLabel[k].toLowerCase()}
      </span>
    ));

  return (
    <HoverCtx.Provider value={{ showHover, scheduleHide }}>
      <section className="tree-pane" aria-label="Connections">
        <div className="tree-search">
          <Search size={14} />
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search connections"
            aria-label="Search connections"
          />
        </div>

        {changes.length > 0 && (
          <div className="change-bar" role="status">
            <div className="change-summary">{summary}</div>
            <div className="change-actions">
              <button className="btn-quiet" onClick={() => setReviewing((r) => !r)} aria-pressed={reviewing}>
                {reviewing ? "Show all" : "Review"}
              </button>
              <button className="btn-quiet" onClick={onAckAll}>
                <Check size={14} /> Acknowledge all
              </button>
            </div>
          </div>
        )}

        <div className="tree-box" ref={boxRef}>
          {size.height > 0 && (
            <Tree<TreeNode>
              data={data}
              width={size.width}
              height={size.height}
              rowHeight={28}
              indent={16}
              openByDefault
              searchTerm={search}
              searchMatch={(n: NodeApi<TreeNode>, term: string) => {
                const t = term.toLowerCase();
                const d = n.data;
                return (
                  d.name.toLowerCase().includes(t) || (d.type === "connection" && d.endpoint.toLowerCase().includes(t))
                );
              }}
              disableDrop={({ parentNode }) => parentNode.data?.type === "connection"}
              onActivate={(n) => n.data.type === "connection" && onOpen(n.data)}
              onMove={({ dragIds, parentId, index }) => dragIds.forEach((id) => onMove(id, parentId, index))}
            >
              {Row}
            </Tree>
          )}
        </div>

        {hover?.node.change &&
          createPortal(
            <HoverCard
              node={hover.node}
              change={hover.node.change}
              top={hover.top}
              left={hover.left}
              onEnter={() => window.clearTimeout(hideTimer.current)}
              onLeave={scheduleHide}
              onAck={() => {
                onAck(hover.node.id);
                setHover(null);
              }}
              onRevert={(opId) => {
                onRevert(opId);
                setHover(null);
              }}
            />,
            document.body,
          )}
      </section>
    </HoverCtx.Provider>
  );
}

function HoverCard(props: {
  node: TreeNode;
  change: PendingChange;
  top: number;
  left: number;
  onEnter(): void;
  onLeave(): void;
  onAck(): void;
  onRevert(opId: string): void;
}) {
  const { node, change } = props;
  const revertLabel = (k: ChangeKind) => (k === "deleted" ? "Restore" : k === "reverted" ? "Undo revert" : "Revert");
  return (
    <div
      className={`hover-card hc-${change.kind}`}
      style={{ top: props.top, left: props.left }}
      onMouseEnter={props.onEnter}
      onMouseLeave={props.onLeave}
      role="dialog"
      aria-label={`${changeLabel[change.kind]}: ${node.name}`}
    >
      <header>
        <span className={`hc-kind chg-${change.kind}`}>
          <ChangeGlyph kind={change.kind} /> {changeLabel[change.kind]}
        </span>
        <strong>{node.name}</strong>
      </header>
      <ol className="hc-entries">
        {change.entries.map((e) => (
          <li key={e.opId}>
            <div className="hc-meta">
              {changeLabel[e.kind]} on <b>{e.device}</b>, {relativeTime(e.at)}
            </div>
            {e.fields.length > 0 && (
              <table className="hc-diff">
                <tbody>
                  {e.fields.map((f) => (
                    <tr key={f.field}>
                      <th>{f.field}</th>
                      <td className="before">{f.before ?? "—"}</td>
                      <td className="after">{f.after ?? "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <button className="btn-small" onClick={() => props.onRevert(e.opId)}>
              <Undo2 size={13} /> {revertLabel(e.kind)}
            </button>
          </li>
        ))}
      </ol>
      <footer>
        <button className="btn-small btn-primary" onClick={props.onAck}>
          <Check size={13} /> Acknowledge
        </button>
      </footer>
    </div>
  );
}
