import assert from "node:assert/strict";
import { test } from "node:test";
import { OSC52_MAX, openableUrl, osc52Text } from "./termlinks.ts";

const b64 = (s: string) => Buffer.from(s, "utf8").toString("base64");

test("OSC 52 copies the decoded text", () => {
  const url = "https://claude.ai/oauth/authorize?code=true&client_id=abc&state=xyz";
  assert.equal(osc52Text(`c;${b64(url)}`), url);
  assert.equal(osc52Text(`;${b64("hi")}`), "hi");
  assert.equal(osc52Text(`pc;${b64("héllo ✓")}`), "héllo ✓");
});

test("OSC 52 never reads the clipboard and ignores junk", () => {
  assert.equal(osc52Text("c;?"), null);
  assert.equal(osc52Text("c;"), null);
  assert.equal(osc52Text("no separator"), null);
  assert.equal(osc52Text("c;not*base64"), null);
  assert.equal(osc52Text(`c;${Buffer.from([0xff, 0xfe]).toString("base64")}`), null, "not UTF-8");
  assert.equal(osc52Text(`c;${b64("x".repeat(OSC52_MAX + 3))}`), null, "too big");
});

test("only web and mail links open", () => {
  assert.ok(openableUrl("https://claude.ai/oauth/authorize?x=1"));
  assert.ok(openableUrl("http://10.1.1.1/"));
  assert.ok(openableUrl("mailto:noc@example.com"));
  for (const bad of ["file:///C:/Windows/system32/calc.exe", "javascript:alert(1)", "ms-settings:", "nonsense"]) {
    assert.equal(openableUrl(bad), false, bad);
  }
});
