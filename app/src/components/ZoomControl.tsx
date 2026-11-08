import { useEffect, useState } from "react";
import { RotateCcw, ZoomIn, ZoomOut } from "lucide-react";
import { usePrefs } from "../prefs";
import { FONT_DEFAULT, FONT_MAX, FONT_MIN, clampFont, fontToPercent, percentToFont, stepFont } from "../zoom";

/** Terminal text size for every tab: smaller, the level (type your own), bigger, reset. */
export function ZoomControl() {
  const { prefs, update } = usePrefs();
  const size = clampFont(prefs.fontSize);
  const shown = `${fontToPercent(size)}%`;
  const [text, setText] = useState(shown);
  const [editing, setEditing] = useState(false);
  useEffect(() => {
    if (!editing) setText(shown);
  }, [shown, editing]);

  const set = (next: number) => update({ fontSize: clampFont(next) });
  const commit = () => {
    const next = percentToFont(text);
    if (next !== null) set(next);
    setEditing(false);
    setText(`${fontToPercent(next ?? size)}%`);
  };

  return (
    <div className="zoom-control" role="group" aria-label="Terminal zoom">
      <button
        className="icon-btn"
        aria-label="Zoom out"
        title="Zoom out (Ctrl+scroll down, or pinch)"
        disabled={size <= FONT_MIN}
        onClick={() => set(stepFont(size, -1))}
      >
        <ZoomOut size={15} />
      </button>
      <input
        className="zoom-level"
        value={text}
        aria-label="Zoom level"
        title="Zoom for every terminal. Type a level (60–250%) and press Enter."
        inputMode="numeric"
        onFocus={(e) => {
          setEditing(true);
          e.currentTarget.select();
        }}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            commit();
            e.currentTarget.blur();
          } else if (e.key === "Escape") {
            setEditing(false);
            setText(shown);
            e.currentTarget.blur();
          } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
            e.preventDefault();
            set(stepFont(size, e.key === "ArrowUp" ? 1 : -1));
            setEditing(false);
          }
        }}
      />
      <button
        className="icon-btn"
        aria-label="Zoom in"
        title="Zoom in (Ctrl+scroll up, or spread)"
        disabled={size >= FONT_MAX}
        onClick={() => set(stepFont(size, 1))}
      >
        <ZoomIn size={15} />
      </button>
      <button
        className="icon-btn"
        aria-label="Reset zoom"
        title="Reset zoom to 100%"
        disabled={size === FONT_DEFAULT}
        onClick={() => set(FONT_DEFAULT)}
      >
        <RotateCcw size={13} />
      </button>
    </div>
  );
}
