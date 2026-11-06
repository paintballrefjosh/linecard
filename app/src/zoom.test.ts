import { test } from "node:test";
import assert from "node:assert/strict";
import { clampFont, FONT_MAX, FONT_MIN, WheelZoom } from "./zoom.ts";

test("a mouse wheel notch is one step, up is bigger", () => {
  const z = new WheelZoom();
  assert.equal(z.steps(-100), 1);
  assert.equal(z.steps(100), -1);
  assert.equal(z.steps(3, 1), -1, "line mode counts as notches");
});

test("pinch deltas add up before changing the size", () => {
  const z = new WheelZoom();
  let total = 0;
  for (let i = 0; i < 10; i++) total += z.steps(-5);
  assert.equal(total, 2, "50 units of spread is two steps");
  // Reversing direction undoes what was pending first.
  assert.equal(z.steps(5), 0);
});

test("sizes stay in range", () => {
  assert.equal(clampFont(2), FONT_MIN);
  assert.equal(clampFont(99), FONT_MAX);
  assert.equal(clampFont(13.4), 13);
});
