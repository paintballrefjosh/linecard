import assert from "node:assert/strict";
import { test } from "node:test";
import { describe, isDue, timeToday, type Schedule } from "./schedule.ts";

const at = (h: number, m = 0, day = 15) => new Date(2026, 10, day, h, m, 0, 0);
const ms = (d: Date) => d.getTime();

test("an interval counts from startup, then from each run", () => {
  const rule: Schedule = { id: "a", script: "x.py", every: 30 };
  const start = ms(at(9, 0));
  assert.equal(isDue(rule, at(9, 29), null, start), false);
  assert.equal(isDue(rule, at(9, 30), null, start), true);
  assert.equal(isDue(rule, at(9, 45), ms(at(9, 30)), start), false);
  assert.equal(isDue(rule, at(10, 0), ms(at(9, 30)), start), true);
});

test("a daily run happens once, at or after its time", () => {
  const rule: Schedule = { id: "a", script: "x.py", at: "06:00" };
  const start = ms(at(5, 0));
  assert.equal(isDue(rule, at(5, 59), null, start), false);
  assert.equal(isDue(rule, at(6, 0), null, start), true);
  assert.equal(isDue(rule, at(6, 5), ms(at(6, 0)), start), false, "already ran today");
  assert.equal(isDue(rule, at(6, 0, 16), ms(at(6, 0)), start), true, "and again the next day");
});

test("a daily run missed while the app was closed is not made up", () => {
  const rule: Schedule = { id: "a", script: "x.py", at: "06:00" };
  assert.equal(isDue(rule, at(8, 0), null, ms(at(7, 30))), false, "started after the time");
  assert.equal(isDue(rule, at(8, 0), null, ms(at(5, 0))), true, "was open, slept through it");
});

test("bad rules never run", () => {
  assert.equal(isDue({ id: "a", script: "x", every: 0 }, at(9), null, 0), false);
  assert.equal(isDue({ id: "a", script: "x", at: "25:00" }, at(9), null, 0), false);
  assert.equal(isDue({ id: "a", script: "x" }, at(9), null, 0), false);
  assert.equal(timeToday("6:5", at(9)), null);
  assert.equal(timeToday("6:05", at(9))?.getHours(), 6);
});

test("rules read back plainly", () => {
  assert.equal(describe({ id: "a", script: "x", every: 1 }), "every minute");
  assert.equal(describe({ id: "a", script: "x", every: 15 }), "every 15 minutes");
  assert.equal(describe({ id: "a", script: "x", at: "06:00" }), "daily at 06:00");
});
