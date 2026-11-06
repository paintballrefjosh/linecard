/** Terminal text size: Ctrl+scroll wheel and pinch/spread make it bigger or smaller. */

export const FONT_DEFAULT = 13;
export const FONT_MIN = 8;
export const FONT_MAX = 32;

export const clampFont = (size: number) => Math.min(FONT_MAX, Math.max(FONT_MIN, Math.round(size)));

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
