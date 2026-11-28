import { NO_TAB_COLOR, swatches } from "../tabColor";
import type { InheritedValue } from "../types";

interface Props {
  /** null = from the folder; undefined = nothing selected (mixed). */
  value: string | null | undefined;
  /** What applies when nothing is chosen here. Unused with `bulk`. */
  inherited?: InheritedValue | null;
  /** Editing several connections, whose folders may differ. */
  bulk?: boolean;
  onChange(v: string | null): void;
}

/** The tab-color swatches: inherit, no color, or a color. "No color" is a
 *  choice of its own only where a folder's color could otherwise apply. */
export function TabColorPicker({ value, inherited, bulk = false, onChange }: Props) {
  const from = inherited && inherited.value !== NO_TAB_COLOR ? inherited : null;
  const offerNone = bulk || !!from;
  const inheritLabel = bulk
    ? "Color from the folder"
    : from
      ? `Inherited: ${from.value} (from ${from.from})`
      : inherited
        ? `No color (from ${inherited.from})`
        : "No color";
  const swatch = (v: string | null, label: string, look: string, checked: boolean, background?: string) => (
    <button
      key={v ?? "inherit"}
      type="button"
      role="radio"
      aria-checked={checked}
      aria-label={label}
      title={label}
      className={`swatch ${look}`}
      style={background ? { background } : undefined}
      onClick={() => onChange(v)}
    />
  );
  return (
    <div className="swatches" role="radiogroup" aria-label="Tab color">
      {swatch(
        null,
        inheritLabel,
        offerNone ? "swatch-inherit" : "swatch-none",
        // With no folder color, a saved "no color" looks the same as this.
        value === null || (!offerNone && value === NO_TAB_COLOR),
        from?.value,
      )}
      {offerNone && swatch(NO_TAB_COLOR, "No color", "swatch-none", value === NO_TAB_COLOR)}
      {swatches.map((c) => swatch(c, c, "", value === c, c))}
    </div>
  );
}
