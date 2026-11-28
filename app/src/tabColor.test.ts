import assert from "node:assert/strict";
import { test } from "node:test";
import { NO_TAB_COLOR, connectionColors, inheritTabColors, tabColorOf } from "./tabColor.ts";
import type { ConnectionNode, TreeNode } from "./types.ts";

const conn = (id: string, tabColor?: string): ConnectionNode => ({
  type: "connection",
  id,
  name: id,
  kind: "ssh",
  endpoint: `${id}:22`,
  tabColor,
});
const folder = (id: string, children: TreeNode[]): TreeNode => ({ type: "folder", id, name: id, children });

const tree = [folder("dc", [conn("a"), conn("b", NO_TAB_COLOR), folder("quiet", [conn("c"), conn("d", "#3a7bd5")])])];
const folderColors: Record<string, string> = { dc: "#d64545", quiet: NO_TAB_COLOR };
const own = (n: TreeNode) => (n.type === "folder" ? folderColors[n.id] : n.tabColor);

test("colors pass down until something below overrides them", () => {
  const shown = inheritTabColors(tree, own);
  assert.deepEqual(Object.fromEntries(connectionColors(shown)), {
    a: "#d64545",
    b: undefined,
    c: undefined,
    d: "#3a7bd5",
  });
  const dc = shown[0];
  assert.ok(dc.type === "folder");
  assert.equal(dc.tabColor, "#d64545");
  assert.equal(dc.children[2].tabColor, undefined);
});

test("an open tab takes the connection's color of now", () => {
  const colors = connectionColors([conn("a", "#3fa35b"), conn("b")]);
  assert.equal(tabColorOf(colors, conn("a", "#d64545")), "#3fa35b");
  assert.equal(tabColorOf(colors, conn("b", "#d64545")), undefined, "a removed color goes away");
  assert.equal(tabColorOf(colors, conn("gone", "#d64545")), "#d64545", "deleted since: as it was");
});
