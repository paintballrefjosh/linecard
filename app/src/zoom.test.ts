import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampFont,
  fontToPercent,
  FONT_DEFAULT,
  FONT_MAX,
  FONT_MIN,
  percentToFont,
  stepFont,
  WheelZoom,
  ZOOM_MAX,
  ZOOM_MIN,
} from "./zoom.ts";

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
  assert.equal(clampFont(Number.NaN), FONT_DEFAULT);
  assert.equal(clampFont(13.456), 13.46, "kept to 1/100 pt");
});

test("zoom level is a percentage of the default size", () => {
  assert.equal(fontToPercent(FONT_DEFAULT), 100);
  assert.equal(percentToFont("100%"), FONT_DEFAULT);
  assert.equal(percentToFont(" 200 % "), 26);
  assert.equal(percentToFont("150"), 19.5, "no % sign is fine");
  assert.equal(percentToFont("1000"), FONT_MAX, "clamped");
  assert.equal(percentToFont("5"), FONT_MIN, "clamped");
  assert.equal(percentToFont("big"), null);
  assert.equal(percentToFont("0"), null);
  // Whatever level you type is the level the box then shows.
  for (let p = ZOOM_MIN; p <= ZOOM_MAX; p++) assert.equal(fontToPercent(percentToFont(`${p}%`)!), p);
});

test("steps land on whole 10% levels", () => {
  const level = (size: number) => fontToPercent(size);
  assert.equal(level(stepFont(FONT_DEFAULT, 1)), 110);
  assert.equal(level(stepFont(FONT_DEFAULT, -1)), 90);
  assert.equal(level(stepFont(FONT_DEFAULT, 3)), 130);
  assert.equal(level(stepFont(percentToFont("115")!, 1)), 120);
  assert.equal(level(stepFont(percentToFont("115")!, -1)), 110);
  assert.equal(stepFont(FONT_MAX, 1), FONT_MAX);
  assert.equal(stepFont(FONT_MIN, -1), FONT_MIN);
  // An old whole-point size from before levels still steps cleanly.
  assert.equal(level(stepFont(14, 1)), 110, "14 pt is 108%");
});
