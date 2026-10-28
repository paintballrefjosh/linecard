import type { LayoutId } from "../types";

/** Each layout drawn as the panes it produces (rects in an 18×14 box). */
const LAYOUTS: { id: LayoutId; label: string; key: string; rects: [number, number, number, number][] }[] = [
  { id: "1", label: "One pane", key: "1", rects: [[1, 1, 16, 12]] },
  {
    id: "2v",
    label: "Two panes side by side",
    key: "2",
    rects: [
      [1, 1, 7.5, 12],
      [9.5, 1, 7.5, 12],
    ],
  },
  {
    id: "2h",
    label: "Two panes stacked",
    key: "3",
    rects: [
      [1, 1, 16, 5.5],
      [1, 7.5, 16, 5.5],
    ],
  },
  {
    id: "3c",
    label: "Three columns",
    key: "4",
    rects: [
      [1, 1, 4.6, 12],
      [6.7, 1, 4.6, 12],
      [12.4, 1, 4.6, 12],
    ],
  },
  {
    id: "1+2",
    label: "One large, two stacked",
    key: "5",
    rects: [
      [1, 1, 9, 12],
      [11, 1, 6, 5.5],
      [11, 7.5, 6, 5.5],
    ],
  },
  {
    id: "4",
    label: "Four panes",
    key: "6",
    rects: [
      [1, 1, 7.5, 5.5],
      [9.5, 1, 7.5, 5.5],
      [1, 7.5, 7.5, 5.5],
      [9.5, 7.5, 7.5, 5.5],
    ],
  },
];

export const layoutShortcut = (key: string) => LAYOUTS.find((l) => l.key === key)?.id;

export function LayoutPicker({ value, onChange }: { value: LayoutId; onChange(id: LayoutId): void }) {
  return (
    <div className="layout-picker" role="radiogroup" aria-label="Pane layout">
      {LAYOUTS.map((l) => (
        <button
          key={l.id}
          role="radio"
          aria-checked={value === l.id}
          aria-label={l.label}
          title={`${l.label} (Ctrl+Alt+${l.key})`}
          className={value === l.id ? "is-active" : ""}
          onClick={() => onChange(l.id)}
        >
          <svg viewBox="0 0 18 14" width="22" height="17" aria-hidden="true">
            {l.rects.map(([x, y, w, h], i) => (
              <rect key={i} x={x} y={y} width={w} height={h} rx="1.2" />
            ))}
          </svg>
        </button>
      ))}
    </div>
  );
}
