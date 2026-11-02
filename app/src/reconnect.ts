/** Ways to start a tab's session over, by panel id, so the tab's own menu
 *  can offer Reconnect. Each session view registers itself while it is open. */
const fns = new Map<string, () => void>();

export const reconnect = {
  register(panelId: string, fn: (() => void) | null) {
    if (fn) fns.set(panelId, fn);
    else fns.delete(panelId);
  },
  run(panelId: string) {
    fns.get(panelId)?.();
  },
};
