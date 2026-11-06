import { useRef, useState } from "react";
import { Terminal } from "lucide-react";
import type { LocalShell } from "../types";
import { ContextMenu, type MenuItem } from "./ContextMenu";
import { KindIcon } from "./icons";

/** Toolbar button: open a shell on this computer in a new tab. */
export function ShellMenu({ shells, onOpen }: { shells: LocalShell[] | null; onOpen(shell: LocalShell): void }) {
  const button = useRef<HTMLButtonElement>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  const items: MenuItem[] =
    shells === null
      ? [{ label: "Looking for shells…", disabled: true, onSelect: () => {} }]
      : shells.length === 0
        ? [{ label: "No shells found on this computer", disabled: true, onSelect: () => {} }]
        : shells.map((s, i) => ({
            label: s.name,
            icon: <KindIcon kind="local" size={15} />,
            shortcut: i === 0 ? "Ctrl+Shift+T" : undefined,
            onSelect: () => onOpen(s),
          }));

  return (
    <>
      <button
        ref={button}
        className="toolbar-btn"
        title="New local shell (Ctrl+Shift+T)"
        aria-label="New local shell"
        aria-haspopup="menu"
        onClick={() => {
          const r = button.current!.getBoundingClientRect();
          setMenu({ x: r.right - 240, y: r.bottom + 4 });
        }}
      >
        <Terminal size={17} />
      </button>
      {menu && <ContextMenu x={menu.x} y={menu.y} label="Local shells" items={items} onClose={() => setMenu(null)} />}
    </>
  );
}
