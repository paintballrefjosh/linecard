import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { Tree, type NodeApi, type NodeRendererProps, type TreeApi } from "react-arborist";
import {
  Check,
  ChevronDown,
  ChevronRight,
  ChevronsDownUp,
  ChevronsUpDown,
  Copy,
  FileInput,
  Folder,
  FolderOpen,
  FolderPlus,
  Pencil,
  Play,
  Plus,
  Search,
  TextCursorInput,
  Trash2,
} from "lucide-react";
import type { ChangeKind, ConnectionNode, PendingChange, TreeNode } from "../types";
import { ContextMenu, type MenuItem } from "./ContextMenu";
import { ConfirmDialog } from "./Dialog";
import { ChangeGlyph, KindIcon, Undo2, changeLabel } from "./icons";

/** Tree edits; the App owns the connection editor dialog. */
export interface TreeActions {
  newConnection(folderId: string | null): void;
  editConnection(id: string): void;
  createFolder(parentId: string | null, name: string): Promise<string>;
  rename(id: string, name: string): Promise<void>;
  remove(id: string): Promise<void>;
  duplicate(id: string): Promise<string>;
  importConnections(): void;
}

interface Props {
  tree: TreeNode[];
  actions: TreeActions;
  onOpen(conn: ConnectionNode): void;
  onAck(id: string): void;
  onAckAll(): void;
  onRevert(opId: string): void;
  onMove(id: string, parentId: string | null, index: number): void;
}

/** Folder a node lives in (null = top level). */
const parentIdOf = (n: NodeApi<TreeNode>) => (n.level > 0 && n.parent ? n.parent.id : null);

function countBelow(node: TreeNode): number {
  return node.type === "folder" ? node.children.reduce((n, c) => n + 1 + countBelow(c), 0) : 0;
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

interface RowCtxValue {
  showHover(node: TreeNode, el: HTMLElement): void;
  scheduleHide(): void;
  openMenu(x: number, y: number, node: NodeApi<TreeNode> | null): void;
  onOpen(conn: ConnectionNode): void;
}

const RowCtx = createContext<RowCtxValue>({
  showHover: () => {},
  scheduleHide: () => {},
  openMenu: () => {},
  onOpen: () => {},
});

function RenameInput({ node }: { node: NodeApi<TreeNode> }) {
  return (
    <input
      className="tree-rename"
      defaultValue={node.data.name}
      aria-label="New name"
      autoFocus
      onFocus={(e) => e.currentTarget.select()}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onBlur={(e) => node.submit(e.currentTarget.value)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") node.submit(e.currentTarget.value);
        else if (e.key === "Escape") node.reset();
      }}
    />
  );
}

function Row({ node, style, dragHandle }: NodeRendererProps<TreeNode>) {
  const { showHover, scheduleHide, openMenu, onOpen } = useContext(RowCtx);
  const d = node.data;
  const change = d.change;
  const below = d.type === "folder" && !node.isOpen ? changedBelow(d) : 0;
  const tabColor = d.type === "connection" ? d.tabColor : undefined;
  return (
    <div
      ref={dragHandle}
      style={tabColor ? ({ ...style, "--tab-color": tabColor } as CSSProperties) : style}
      className={`tree-row ${tabColor ? "has-tab-color" : ""} ${node.isSelected ? "is-selected" : ""} ${change ? `chg chg-${change.kind}` : ""}`}
      onMouseEnter={change ? (e) => showHover(d, e.currentTarget) : undefined}
      onMouseLeave={change ? scheduleHide : undefined}
      onClick={() => d.type === "folder" && !node.isEditing && node.toggle()}
      onDoubleClick={() => d.type === "connection" && !node.isEditing && onOpen(d)}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (!node.isSelected) node.select();
        openMenu(e.clientX, e.clientY, node);
      }}
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
      {node.isEditing ? <RenameInput node={node} /> : <span className="tree-name">{d.name}</span>}
      {d.type === "connection" && !node.isEditing && <span className="tree-endpoint">{d.endpoint}</span>}
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

export function ConnectionTree({ tree, actions, onOpen, onAck, onAckAll, onRevert, onMove }: Props) {
  const [search, setSearch] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const [hover, setHover] = useState<HoverState | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; node: NodeApi<TreeNode> | null } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<TreeNode | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** A just-created folder to put into rename mode once it appears. */
  const pendingRename = useRef<string | null>(null);
  const treeApi = useRef<TreeApi<TreeNode> | null>(null);
  const hideTimer = useRef<number | undefined>(undefined);
  const [boxRef, size] = useSize<HTMLDivElement>();

  useEffect(() => {
    const id = pendingRename.current;
    const node = id ? treeApi.current?.get(id) : null;
    if (!node) return;
    pendingRename.current = null;
    node.openParents();
    node.select();
    node.edit();
  }, [tree]);

  const report = (p: Promise<unknown>) =>
    p.catch((e) => {
      setError(String(e));
      window.setTimeout(() => setError(null), 4000);
    });

  const newFolder = (parentId: string | null) =>
    report(
      actions.createFolder(parentId, "New folder").then((id) => {
        pendingRename.current = id;
      }),
    );

  const menuItems = (n: NodeApi<TreeNode> | null): MenuItem[] => {
    const create = (folderId: string | null): MenuItem[] => [
      { label: "New connection…", icon: <Plus size={15} />, onSelect: () => actions.newConnection(folderId) },
      { label: "New folder", icon: <FolderPlus size={15} />, onSelect: () => newFolder(folderId) },
    ];
    if (!n) {
      return [
        ...create(null),
        "separator",
        { label: "Import connections…", icon: <FileInput size={15} />, onSelect: actions.importConnections },
        "separator",
        { label: "Expand all", icon: <ChevronsUpDown size={15} />, onSelect: () => treeApi.current?.openAll() },
        { label: "Collapse all", icon: <ChevronsDownUp size={15} />, onSelect: () => treeApi.current?.closeAll() },
      ];
    }
    const d = n.data;
    const rename: MenuItem = {
      label: "Rename",
      icon: <TextCursorInput size={15} />,
      shortcut: "F2",
      onSelect: () => n.edit(),
    };
    const del: MenuItem = {
      label: "Delete",
      icon: <Trash2 size={15} />,
      shortcut: "Del",
      danger: true,
      onSelect: () => setConfirmDelete(d),
    };
    if (d.type === "folder") return [...create(d.id), "separator", rename, del];
    return [
      { label: "Connect", icon: <Play size={15} />, shortcut: "Enter", onSelect: () => onOpen(d) },
      { label: "Edit…", icon: <Pencil size={15} />, onSelect: () => actions.editConnection(d.id) },
      rename,
      { label: "Duplicate", icon: <Copy size={15} />, onSelect: () => report(actions.duplicate(d.id)) },
      "separator",
      ...create(parentIdOf(n)),
      "separator",
      del,
    ];
  };

  // Enter connects (arborist would rename), F2 renames, Delete asks first.
  const onKeyDownCapture = (e: KeyboardEvent) => {
    const t = treeApi.current;
    const n = t?.focusedNode;
    if (!t || !n || t.isEditing || (e.target as Element).closest("input")) return;
    if (e.key === "Enter") {
      if (n.data.type === "connection") onOpen(n.data);
      else n.toggle();
    } else if (e.key === "F2") n.edit();
    else if (e.key === "Delete") setConfirmDelete(n.data);
    else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      n.select();
      // Wait a frame for the selection to render, then anchor under that row.
      requestAnimationFrame(() => {
        const r = t.listEl.current?.querySelector(".tree-row.is-selected")?.getBoundingClientRect();
        setMenu({ x: (r?.left ?? 0) + 24, y: r?.bottom ?? 0, node: n });
      });
    } else return;
    e.preventDefault();
    e.stopPropagation();
  };

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
    <RowCtx.Provider value={{ showHover, scheduleHide, openMenu: (x, y, node) => setMenu({ x, y, node }), onOpen }}>
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

        <div
          className="tree-box"
          ref={boxRef}
          onKeyDownCapture={onKeyDownCapture}
          onContextMenu={(e) => {
            // Rows handle their own menu; this is the empty space below them.
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY, node: null });
          }}
        >
          {size.height > 0 && (
            <Tree<TreeNode>
              ref={treeApi}
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
              onRename={({ id, name, node }) => {
                if (name.trim() && name.trim() !== node.data.name) report(actions.rename(id, name));
              }}
              onMove={({ dragIds, parentId, index }) => dragIds.forEach((id) => onMove(id, parentId, index))}
            >
              {Row}
            </Tree>
          )}
        </div>

        {error && (
          <div className="tree-error" role="alert">
            {error}
          </div>
        )}

        {menu && (
          <ContextMenu
            x={menu.x}
            y={menu.y}
            label={menu.node ? `${menu.node.data.name} actions` : "Connections"}
            items={menuItems(menu.node)}
            onClose={() => setMenu(null)}
          />
        )}

        {confirmDelete && (
          <ConfirmDialog
            title={confirmDelete.type === "folder" ? "Delete folder" : "Delete connection"}
            message={
              confirmDelete.type === "folder" && countBelow(confirmDelete) > 0 ? (
                <>
                  Delete <b>{confirmDelete.name}</b> and the {countBelow(confirmDelete)} items inside it?
                </>
              ) : (
                <>
                  Delete <b>{confirmDelete.name}</b>?
                </>
              )
            }
            confirmLabel="Delete"
            danger
            onConfirm={() => actions.remove(confirmDelete.id)}
            onClose={() => setConfirmDelete(null)}
          />
        )}

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
    </RowCtx.Provider>
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
