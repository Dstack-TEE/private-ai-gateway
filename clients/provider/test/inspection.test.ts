import assert from "node:assert/strict";
import test from "node:test";

import { formatAciInspection, type AciInspectionResult } from "../src/inspection.ts";

test("formats the shared receipt history for text-based host adapters", () => {
  const result: AciInspectionResult = {
    action: "receipts",
    receipts: [
      {
        receiptId: "receipt-latest",
        method: "POST",
        path: "/v1/chat/completions",
        status: 200,
        recordedAt: 1_700_000_000_000,
        responseComplete: true,
      },
    ],
  };

  assert.equal(
    formatAciInspection(result),
    "receipt-latest POST /v1/chat/completions HTTP 200 complete",
  );
  assert.equal(
    formatAciInspection({ action: "receipts", receipts: [] }),
    "No ACI receipts have been recorded in this process.",
  );
});
