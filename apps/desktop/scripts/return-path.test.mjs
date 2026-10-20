import assert from "node:assert/strict";
import { test } from "node:test";
import { returnPath } from "../src/renderer/lib/return-path.ts";

const origin = "http://127.0.0.1:4181";

test("sign-in returns to a path of this app, with its search and hash", () => {
  for (const path of ["/", "/usage", "/usage?agent=codex&rows=50", "/settings#web-ui"]) assert.equal(returnPath(path, origin), path);
});

test("anything that would leave the app, or isn't a path, returns to the default page", () => {
  for (const value of ["//evil.example", "/\\evil.example/", "/\t/evil.example", "https://evil.example/", "usage", "", undefined, ["/usage"]]) {
    assert.equal(returnPath(value, origin), undefined, JSON.stringify(value));
  }
});
