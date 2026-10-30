import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from "react";
import { GripVertical, ListTree, Pin, PinOff } from "lucide-react";

export type Dock = "left" | "right" | "top" | "bottom";

const KEYS = {
  width: "linecard.sidebarWidth",
  height: "linecard.sidebarHeight",
  dock: "linecard.panelDock",
  pinned: "linecard.panelPinned",
};
const DEFAULT = { width: 260, height: 240 };
const MIN = { width: 180, height: 120 };

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable: keep in memory only */
  }
}

const vertical = (d: Dock) => d === "left" || d === "right";

/** Keep the panel within the window, leaving room for the workspace. */
function clamp(size: number, dock: Dock): number {
  const v = vertical(dock);
  const room = v ? window.innerWidth - 48 - 320 : window.innerHeight - 220;
  const max = Math.max(v ? MIN.width : MIN.height, Math.min(v ? 640 : 600, room));
  return Math.round(Math.min(Math.max(size, v ? MIN.width : MIN.height), max));
}

/** Where the connection panel sits, whether it's pinned, and its size —
 *  remembered on this computer. */
export function usePanelLayout() {
  const [dock, setDockState] = useState<Dock>(() => {
    const d = read(KEYS.dock);
    return d === "right" || d === "top" || d === "bottom" ? d : "left";
  });
  const [pinned, setPinnedState] = useState(() => read(KEYS.pinned) !== "false");
  const [sizes, setSizes] = useState(() => ({
    width: Number(read(KEYS.width)) || DEFAULT.width,
    height: Number(read(KEYS.height)) || DEFAULT.height,
  }));
  const [, rerender] = useState(0);
  useEffect(() => {
    const onResize = () => rerender((n) => n + 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const size = clamp(vertical(dock) ? sizes.width : sizes.height, dock);
  const setSize = useCallback(
    (s: number, persist: boolean) => {
      const which = vertical(dock) ? "width" : "height";
      const c = clamp(s, dock);
      setSizes((cur) => ({ ...cur, [which]: c }));
      if (persist) write(KEYS[which], String(c));
    },
    [dock],
  );
  const setDock = (d: Dock) => {
    setDockState(d);
    write(KEYS.dock, d);
  };
  const setPinned = (p: boolean) => {
    setPinnedState(p);
    write(KEYS.pinned, String(p));
  };
  const resetSize = () => setSize(vertical(dock) ? DEFAULT.width : DEFAULT.height, true);
  return { dock, pinned, size, setSize, resetSize, setDock, setPinned };
}

/** Drag handle on the panel's inner edge. Double-click resets the size;
 *  arrow keys nudge it when focused. */
function Resizer({
  dock,
  size,
  onChange,
  onReset,
}: {
  dock: Dock;
  size: number;
  onChange(s: number, persist: boolean): void;
  onReset(): void;
}) {
  const drag = useRef<{ at: number; size: number } | null>(null);
  const v = vertical(dock);
  // Moving toward the workspace grows the panel.
  const sign = dock === "left" || dock === "top" ? 1 : -1;
  const pos = (e: PointerEvent) => (v ? e.clientX : e.clientY);

  const down = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { at: pos(e), size };
    document.body.classList.add(v ? "is-resizing" : "is-resizing-rows");
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    if (drag.current) onChange(drag.current.size + sign * (pos(e) - drag.current.at), false);
  };
  const up = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    onChange(drag.current.size + sign * (pos(e) - drag.current.at), true);
    drag.current = null;
    document.body.classList.remove("is-resizing", "is-resizing-rows");
  };
  const key = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = (e.shiftKey ? 64 : 16) * sign;
    const grow = v ? "ArrowRight" : "ArrowDown";
    const shrink = v ? "ArrowLeft" : "ArrowUp";
    if (e.key === grow) onChange(size + step, true);
    else if (e.key === shrink) onChange(size - step, true);
    else return;
    e.preventDefault();
  };
  return (
    <div
      className="sidebar-resizer"
      role="separator"
      aria-orientation={v ? "vertical" : "horizontal"}
      aria-label="Resize connections pane"
      aria-valuenow={size}
      tabIndex={0}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
      onDoubleClick={onReset}
      onKeyDown={key}
    />
  );
}

/** Which edge of `rect` the point is nearest to. */
function nearestEdge(rect: DOMRect, x: number, y: number): Dock {
  const d: [Dock, number][] = [
    ["left", (x - rect.left) / rect.width],
    ["right", (rect.right - x) / rect.width],
    ["top", (y - rect.top) / rect.height],
    ["bottom", (rect.bottom - y) / rect.height],
  ];
  return d.sort((a, b) => a[1] - b[1])[0][0];
}

interface Props {
  layout: ReturnType<typeof usePanelLayout>;
  /** Auto-hide: shown right now. */
  peek: boolean;
  onPeek(show: boolean): void;
  header: ReactNode;
  children: ReactNode;
  /** The workspace and everything beside the panel. */
  main: ReactNode;
}

/** The connections panel: docked on any side of the window, pinned or
 *  auto-hidden like SecureCRT's session manager. */
export function ConnectionPanel({ layout, peek, onPeek, header, children, main }: Props) {
  const { dock, pinned, size } = layout;
  const shell = useRef<HTMLDivElement>(null);
  const [target, setTarget] = useState<Dock | null>(null);
  const dragging = useRef<{ x: number; y: number; started: boolean } | null>(null);
  const hideTimer = useRef<number | undefined>(undefined);

  const showSoon = () => {
    window.clearTimeout(hideTimer.current);
    onPeek(true);
  };
  const hideSoon = () => {
    if (pinned || dragging.current) return;
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => onPeek(false), 450);
  };
  useEffect(() => () => window.clearTimeout(hideTimer.current), []);

  // Dragging the title bar moves the panel to another side.
  const grab = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || (e.target as Element).closest("button")) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    dragging.current = { x: e.clientX, y: e.clientY, started: false };
  };
  const drag = (e: PointerEvent<HTMLDivElement>) => {
    const d = dragging.current;
    const rect = shell.current?.getBoundingClientRect();
    if (!d || !rect) return;
    if (!d.started && Math.hypot(e.clientX - d.x, e.clientY - d.y) < 6) return;
    d.started = true;
    setTarget(nearestEdge(rect, e.clientX, e.clientY));
  };
  const drop = () => {
    if (dragging.current?.started && target) layout.setDock(target);
    dragging.current = null;
    setTarget(null);
  };
  useEffect(() => {
    if (!target) return;
    const esc = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        dragging.current = null;
        setTarget(null);
      }
    };
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, [target]);

  return (
    <div
      ref={shell}
      className={`shell dock-${dock} ${pinned ? "is-pinned" : "is-autohide"} ${peek ? "is-peek" : ""}`}
      style={{ "--panel-size": `${size}px` } as CSSProperties}
    >
      {!pinned && (
        <button
          className="panel-edge"
          onMouseEnter={showSoon}
          onMouseLeave={hideSoon}
          onClick={() => onPeek(!peek)}
          aria-expanded={peek}
          title="Connections"
        >
          <ListTree size={15} />
          <span>Connections</span>
        </button>
      )}
      <aside className="sidebar" onMouseEnter={showSoon} onMouseLeave={hideSoon}>
        <div
          className="panel-head"
          onPointerDown={grab}
          onPointerMove={drag}
          onPointerUp={drop}
          onPointerCancel={drop}
          title="Drag to dock on another side of the window"
        >
          <GripVertical size={14} className="panel-grip" aria-hidden />
          {header}
          <button
            className="icon-btn panel-pin"
            onClick={() => {
              layout.setPinned(!pinned);
              onPeek(false);
            }}
            aria-pressed={pinned}
            aria-label={pinned ? "Auto-hide the connections pane" : "Pin the connections pane"}
            title={pinned ? "Auto-hide (unpin)" : "Pin open"}
          >
            {pinned ? <Pin size={15} /> : <PinOff size={15} />}
          </button>
        </div>
        {children}
        {(pinned || peek) && <Resizer dock={dock} size={size} onChange={layout.setSize} onReset={layout.resetSize} />}
      </aside>
      {main}
      {target && (
        <div className="dock-targets" aria-hidden>
          {(["left", "right", "top", "bottom"] as Dock[]).map((d) => (
            <div key={d} className={`dock-target dock-target-${d} ${d === target ? "is-target" : ""}`}>
              <span>{d === dock ? "Here" : `Dock ${d}`}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
