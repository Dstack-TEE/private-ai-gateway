import assert from "node:assert/strict";
import { test } from "node:test";
import { returnPath } from "../src/renderer/lib/return-path.ts";

test("sign-in returns to a path of this app, with its search and hash", () => {
  for (const path of ["/", "/usage", "/usage?agent=codex&rows=50", "/settings#web-ui"]) assert.equal(returnPath(path), path);
});

test("anything that could leave the app, or isn't a path, returns to the default page", () => {
  for (const value of [
    "//evil.example", "/\\evil.example/", "/\\/evil.example", "/usage\\..\\x", "\\\\evil.example",
    "https://evil.example/", "usage", "", "/usage\n", "/\tevil", "/\u0000", "/\u007f", undefined, 1, ["/usage"],
  ]) assert.equal(returnPath(value), undefined, JSON.stringify(value));
});
