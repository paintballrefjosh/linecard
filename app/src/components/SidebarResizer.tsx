import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

const KEY = "linecard.sidebarWidth";
const DEFAULT = 260;
const MIN = 180;
/** Room always left for the rail and the workspace. */
const RESERVED = 48 + 320;

function clamp(w: number): number {
  const max = Math.max(MIN, Math.min(640, window.innerWidth - RESERVED));
  return Math.round(Math.min(Math.max(w, MIN), max));
}

function readWidth(): number {
  try {
    const v = Number(localStorage.getItem(KEY));
    return v > 0 ? clamp(v) : DEFAULT;
  } catch {
    return DEFAULT;
  }
}

function saveWidth(w: number) {
  try {
    localStorage.setItem(KEY, String(w));
  } catch {
    /* storage unavailable: keep in memory only */
  }
}

/** Sidebar width (px), remembered per machine and kept within the window. */
export function useSidebarWidth() {
  const [width, setWidth] = useState(readWidth);
  useEffect(() => {
    const onResize = () => setWidth((w) => clamp(w));
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  const set = useCallback((w: number, persist: boolean) => {
    const c = clamp(w);
    setWidth(c);
    if (persist) saveWidth(c);
  }, []);
  return [width, set] as const;
}

/** Drag handle on the sidebar's right edge. Double-click resets the width;
 *  arrow keys nudge it when focused. */
export function SidebarResizer({ width, onChange }: { width: number; onChange(w: number, persist: boolean): void }) {
  const drag = useRef<{ x: number; w: number } | null>(null);

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, w: width };
    document.body.classList.add("is-resizing");
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (drag.current) onChange(drag.current.w + e.clientX - drag.current.x, false);
  };
  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    onChange(drag.current.w + e.clientX - drag.current.x, true);
    drag.current = null;
    document.body.classList.remove("is-resizing");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    if (e.key === "ArrowLeft") onChange(width - step, true);
    else if (e.key === "ArrowRight") onChange(width + step, true);
    else return;
    e.preventDefault();
  };

  return (
    <div
      className="sidebar-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize connections pane"
      aria-valuenow={width}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onDoubleClick={() => onChange(DEFAULT, true)}
      onKeyDown={onKeyDown}
    />
  );
}
