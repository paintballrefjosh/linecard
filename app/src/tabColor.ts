import type { ConnectionNode, TreeNode } from "./types";

/** Same hues as the tab-color convention: red for production, etc. */
export const swatches = ["#d64545", "#e07b39", "#d4a72c", "#3fa35b", "#2aa3a3", "#3a7bd5", "#8a5cd6", "#c94f9b"];

/** Saved as a tab color to show none even though a folder above sets one
 *  (`NO_TAB_COLOR` in core-model). */
export const NO_TAB_COLOR = "none";

/** The color to draw each saved connection with, by id (undefined = none). */
export function connectionColors(
  tree: TreeNode[],
  out = new Map<string, string | undefined>(),
): Map<string, string | undefined> {
  for (const n of tree) {
    if (n.type === "folder") connectionColors(n.children, out);
    else out.set(n.id, n.tabColor);
  }
  return out;
}

/** A tab's color now. `conn` is as it was when the tab opened, so its color
 *  counts only for a connection the tree doesn't have (deleted since, or never
 *  saved). */
export function tabColorOf(colors: Map<string, string | undefined>, conn: ConnectionNode): string | undefined {
  return colors.has(conn.id) ? colors.get(conn.id) : conn.tabColor;
}

/** The tree with every node's effective color, from what each sets itself.
 *  The app's backend does this in Rust; the browser demo uses this. */
export function inheritTabColors(
  tree: TreeNode[],
  own: (n: TreeNode) => string | null | undefined,
  inherited?: string,
): TreeNode[] {
  return tree.map((n) => {
    const set = own(n) || inherited;
    const tabColor = set === NO_TAB_COLOR ? undefined : set;
    return n.type === "folder"
      ? { ...n, tabColor, children: inheritTabColors(n.children, own, set) }
      : { ...n, tabColor };
  });
}
