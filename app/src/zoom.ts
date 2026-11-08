/** Terminal text size: Ctrl+scroll wheel and pinch/spread make it bigger or smaller. */

export const FONT_DEFAULT = 13;
/** Zoom limits, as a percentage of the default size. */
export const ZOOM_MIN = 60;
export const ZOOM_MAX = 250;
export const FONT_MIN = (FONT_DEFAULT * ZOOM_MIN) / 100;
export const FONT_MAX = (FONT_DEFAULT * ZOOM_MAX) / 100;
/** The buttons, wheel and pinch move in steps of this many percent. */
const ZOOM_STEP = 10;

/** A size in range, to 1/100 pt (sizes follow the zoom level, so they need not be whole). */
export const clampFont = (size: number) =>
  Math.round(Math.min(FONT_MAX, Math.max(FONT_MIN, Number.isFinite(size) ? size : FONT_DEFAULT)) * 100) / 100;

/** The size as a zoom level, where the default size is 100%. */
export const fontToPercent = (size: number) => Math.round((size / FONT_DEFAULT) * 100);

const percentSize = (percent: number) => clampFont((percent / 100) * FONT_DEFAULT);

/** A typed zoom level ("120", "120%", " 85 % ") as a text size, or null if it isn't a number. */
export function percentToFont(text: string): number | null {
  const n = Number.parseFloat(text.replace("%", "").trim());
  if (!Number.isFinite(n) || n <= 0) return null;
  return percentSize(n);
}

/** `steps` zoom steps bigger (or smaller, if negative), landing on whole
 *  10% levels like a browser: 100% -> 110%, and 115% -> 120% or 110%. */
export function stepFont(size: number, steps: number): number {
  if (!steps) return clampFont(size);
  const p = fontToPercent(size);
  const base = steps > 0 ? Math.floor(p / ZOOM_STEP) : Math.ceil(p / ZOOM_STEP);
  return percentSize((base + steps) * ZOOM_STEP);
}

/** How much small (pinch) wheel deltas add up to before the size changes. */
const PINCH_STEP = 24;

/**
 * Turns Ctrl+wheel events into size steps. A mouse wheel notch (a big delta)
 * is one step; a touchpad pinch arrives as many small deltas, which are added
 * up. Scrolling up / spreading makes text bigger.
 */
export class WheelZoom {
  private pending = 0;

  /** Steps to apply for this event: positive = bigger. */
  steps(deltaY: number, deltaMode = 0): number {
    // Lines or pages: each event is a notch.
    if (deltaMode !== 0 || Math.abs(deltaY) >= 50) {
      this.pending = 0;
      return deltaY < 0 ? 1 : deltaY > 0 ? -1 : 0;
    }
    this.pending -= deltaY;
    const steps = Math.trunc(this.pending / PINCH_STEP) || 0;
    this.pending -= steps * PINCH_STEP;
    return steps;
  }
}
