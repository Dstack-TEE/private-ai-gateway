import assert from "node:assert/strict";
import test from "node:test";

import { startDeviceAuthorization } from "../src/device-auth.ts";

test("completes a validated RFC 8628 device flow", async () => {
  const requests: Request[] = [];
  const responses = [
    Response.json({
      device_code: "device-secret",
      user_code: "ABCD-EFGH",
      verification_uri: "https://cloud.example/cli/verify",
      verification_uri_complete: "https://cloud.example/cli/verify?code=ABCD-EFGH",
      expires_in: 30,
      interval: 0.001,
    }),
    Response.json(
      {
        error: "authorization_pending",
        error_description: "Waiting for approval",
      },
      { status: 400 },
    ),
    Response.json(
      {
        detail: {
          error: "authorization_pending",
          error_description: "Waiting for approval",
        },
      },
      { status: 400 },
    ),
    Response.json({ access_token: "llm-key", expires_in: null, redpill_key_id: 42 }),
  ];
  const fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    requests.push(new Request(input, init));
    const response = responses.shift();
    if (!response) throw new Error("unexpected request");
    return response;
  };

  const authorization = await startDeviceAuthorization({
    baseURL: "https://cloud.example",
    clientId: "coding-agent",
    scope: "redpill:api-key",
    fetch,
  });
  const token = await authorization.poll();

  assert.equal(authorization.userCode, "ABCD-EFGH");
  assert.equal(token.accessToken, "llm-key");
  assert.equal(token.keyId, 42);
  assert.equal(requests.length, 4);
  assert.deepEqual(await requests[0]?.json(), {
    client_id: "coding-agent",
    scope: "redpill:api-key",
  });
  assert.deepEqual(await requests[1]?.json(), {
    device_code: "device-secret",
    client_id: "coding-agent",
    grant_type: "urn:ietf:params:oauth:grant-type:device_code",
  });
});
