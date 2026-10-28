import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";

export type MenuItem =
  | {
      label: string;
      icon?: ReactNode;
      shortcut?: string;
      danger?: boolean;
      disabled?: boolean;
      onSelect(): void;
    }
  | "separator";

interface Props {
  x: number;
  y: number;
  items: MenuItem[];
  label: string;
  onClose(): void;
}

/** Right-click menu at (x, y), kept inside the window. Arrow keys move,
 *  Enter activates, Escape / outside click / scroll / blur close it. */
export function ContextMenu({ x, y, items, label, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
    el.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }, [x, y]);

  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const close = () => onClose();
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    window.addEventListener("wheel", close, { passive: true });
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("wheel", close);
    };
  }, [onClose]);

  const onKeyDown = (e: KeyboardEvent) => {
    const buttons = [...(ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const go = (n: number) => buttons[(n + buttons.length) % buttons.length]?.focus();
    if (e.key === "ArrowDown") go(i + 1);
    else if (e.key === "ArrowUp") go(i - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(buttons.length - 1);
    else if (e.key === "Escape" || e.key === "Tab") onClose();
    else return;
    e.preventDefault();
    e.stopPropagation();
  };

  return createPortal(
    <div
      ref={ref}
      className="context-menu"
      role="menu"
      aria-label={label}
      style={pos}
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item, i) =>
        item === "separator" ? (
          <div key={i} className="menu-sep" role="separator" />
        ) : (
          <button
            key={i}
            role="menuitem"
            className={item.danger ? "is-danger" : undefined}
            disabled={item.disabled}
            onClick={() => {
              onClose();
              item.onSelect();
            }}
          >
            <span className="menu-icon">{item.icon}</span>
            <span className="menu-label">{item.label}</span>
            {item.shortcut && <kbd>{item.shortcut}</kbd>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}
