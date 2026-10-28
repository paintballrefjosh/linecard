import { useRef, useState } from "react";
import type { DockviewApi } from "dockview";
import { BookmarkPlus, LayoutDashboard } from "lucide-react";
import { usePrefs, type SavedWorkspace } from "../prefs";
import type { ConnectionNode } from "../types";
import { ContextMenu, type MenuItem } from "./ContextMenu";
import { ConfirmDialog, Modal } from "./Dialog";
import { restoreWorkspace, snapshotWorkspace } from "./Workspace";

/** Toolbar button: save the open tabs as a named workspace, or open one. */
export function WorkspaceMenu({
  dock,
  findConnection,
}: {
  dock: { current: DockviewApi | null };
  findConnection(id: string): ConnectionNode | undefined;
}) {
  const { prefs, update } = usePrefs();
  const button = useRef<HTMLButtonElement>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState("");
  const [confirm, setConfirm] = useState<SavedWorkspace | null>(null);

  const openTabs = () => dock.current?.panels.length ?? 0;

  const open = (w: SavedWorkspace) => {
    if (dock.current) restoreWorkspace(dock.current, w.layout, findConnection);
  };

  const save = () => {
    const n = name.trim();
    if (!n || !dock.current) return;
    const entry: SavedWorkspace = { name: n, savedAt: Date.now(), layout: snapshotWorkspace(dock.current) };
    // Saving under an existing name replaces it.
    update({ workspaces: [...prefs.workspaces.filter((w) => w.name !== n), entry] });
    setNaming(false);
  };

  const items: MenuItem[] = [
    {
      label: "Save open tabs as workspace…",
      icon: <BookmarkPlus size={15} />,
      disabled: openTabs() === 0,
      onSelect: () => {
        setName("");
        setNaming(true);
      },
    },
    ...(prefs.workspaces.length ? (["separator"] as MenuItem[]) : []),
    ...prefs.workspaces.map((w): MenuItem => ({
      label: w.name,
      icon: <LayoutDashboard size={15} />,
      shortcut: `${Object.keys(w.layout.panels).length} tabs`,
      onSelect: () => (openTabs() > 0 ? setConfirm(w) : open(w)),
    })),
  ];

  return (
    <>
      <button
        ref={button}
        className="toolbar-btn"
        title="Workspaces"
        aria-label="Workspaces"
        aria-haspopup="menu"
        onClick={() => {
          const r = button.current!.getBoundingClientRect();
          setMenu({ x: r.right - 240, y: r.bottom + 4 });
        }}
      >
        <LayoutDashboard size={17} />
      </button>
      {menu && <ContextMenu x={menu.x} y={menu.y} label="Workspaces" items={items} onClose={() => setMenu(null)} />}
      {naming && (
        <Modal
          title="Save workspace"
          className="modal-confirm"
          onSubmit={save}
          onClose={() => setNaming(false)}
          actions={
            <>
              <button type="button" className="btn" onClick={() => setNaming(false)}>
                Cancel
              </button>
              <button type="submit" className="btn btn-primary" disabled={!name.trim()}>
                Save
              </button>
            </>
          }
        >
          <p>
            Saves the {openTabs()} open tab{openTabs() === 1 ? "" : "s"} and how they're arranged. Opening it later
            reconnects each one.
          </p>
          <input
            className="settings-input"
            aria-label="Workspace name"
            placeholder="e.g. DC1 core change"
            value={name}
            data-autofocus
            onChange={(e) => setName(e.target.value)}
          />
          {prefs.workspaces.some((w) => w.name === name.trim()) && (
            <p className="form-hint">Replaces the workspace with this name.</p>
          )}
        </Modal>
      )}
      {confirm && (
        <ConfirmDialog
          title="Open workspace"
          message={
            <>
              Close the {openTabs()} open tab{openTabs() === 1 ? "" : "s"} and open <b>{confirm.name}</b>?
            </>
          }
          confirmLabel="Open"
          onConfirm={() => open(confirm)}
          onClose={() => setConfirm(null)}
        />
      )}
    </>
  );
}
