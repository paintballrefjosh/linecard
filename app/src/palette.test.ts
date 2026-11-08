import assert from "node:assert/strict";
import { test } from "node:test";
import { flatten, rank, remember, score, RECENT_MAX, type PaletteItem } from "./palette.ts";
import type { TreeNode } from "./types.ts";

const conn = (id: string, name: string, endpoint: string) =>
  ({ type: "connection", id, name, kind: "ssh", endpoint }) as const;

const tree: TreeNode[] = [
  {
    type: "folder",
    id: "dc1",
    name: "DC1 Fort Worth",
    children: [
      { type: "folder", id: "spine", name: "Spine", children: [conn("s1", "spine01", "10.10.0.1:22")] },
      conn("l1", "leaf01", "10.10.1.1:22"),
    ],
  },
  conn("pi", "home-pi", "192.168.1.20:22"),
];

const names = (items: PaletteItem[]) => items.map((i) => i.conn.name);

test("connections are listed with their folder path", () => {
  const items = flatten(tree);
  assert.deepEqual(
    items.map((i) => [i.conn.name, i.path]),
    [
      ["spine01", "DC1 Fort Worth / Spine"],
      ["leaf01", "DC1 Fort Worth"],
      ["home-pi", ""],
    ],
  );
});

test("matches by name, address, folder, or letters in order", () => {
  const items = flatten(tree);
  assert.deepEqual(names(rank(items, "leaf", [])), ["leaf01"]);
  assert.deepEqual(names(rank(items, "192.168", [])), ["home-pi"]);
  assert.deepEqual(names(rank(items, "fort", [])), ["spine01", "leaf01"].sort());
  assert.deepEqual(names(rank(items, "sp1", [])), ["spine01"]);
  assert.deepEqual(names(rank(items, "nothing-like-it", [])), []);
});

test("every word typed must match", () => {
  const items = flatten(tree);
  assert.deepEqual(names(rank(items, "fort spine", [])), ["spine01"]);
  assert.deepEqual(names(rank(items, "fort pi", [])), ["spine01"], "pi matches letters of spine01, not home-pi's folder");
});

test("a name match beats a folder match, and the start of a name beats its middle", () => {
  const items = flatten(tree);
  const s = (q: string, id: string) => score(items.find((i) => i.conn.id === id)!, q)!;
  assert.ok(s("spine", "s1") > s("worth", "s1"));
  assert.ok(s("home", "pi") > s("pi", "pi"));
});

test("nothing typed lists recent connections; recent use breaks ties", () => {
  const items = flatten(tree);
  assert.deepEqual(names(rank(items, "", ["pi", "gone", "s1"])), ["home-pi", "spine01"]);
  assert.deepEqual(names(rank(items, "01", ["l1"])), ["leaf01", "spine01"]);
  assert.deepEqual(names(rank(items, "01", ["s1"])), ["spine01", "leaf01"]);
});

test("recent keeps the newest first, once each, and stays short", () => {
  assert.deepEqual(remember(["a", "b", "c"], "b"), ["b", "a", "c"]);
  let r: string[] = [];
  for (let i = 0; i < RECENT_MAX + 5; i++) r = remember(r, `c${i}`);
  assert.equal(r.length, RECENT_MAX);
  assert.equal(r[0], `c${RECENT_MAX + 4}`);
});
