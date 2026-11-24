import assert from "node:assert/strict";
import { test } from "node:test";
import { pointInSelection } from "../src/renderer/lib/native-interactions.ts";

// A selection of two lines of text, from x 10 to 110 at y 0–20 and x 10 to 60 at y 20–40.
const selection = (rects, isCollapsed = false) => ({
  isCollapsed,
  rangeCount: 1,
  getRangeAt: () => ({ getClientRects: () => rects }),
});
const lines = [{ left: 10, right: 110, top: 0, bottom: 20 }, { left: 10, right: 60, top: 20, bottom: 40 }];

test("a right-click on the selected text opens the edit menu", () => {
  assert.equal(pointInSelection(selection(lines), 100, 10), true);
  assert.equal(pointInSelection(selection(lines), 30, 30), true);
});

test("a right-click elsewhere, or with nothing selected, does not", () => {
  assert.equal(pointInSelection(selection(lines), 100, 30), false);
  assert.equal(pointInSelection(selection(lines), 200, 10), false);
  assert.equal(pointInSelection(selection(lines, true), 100, 10), false);
  assert.equal(pointInSelection(null, 100, 10), false);
});
