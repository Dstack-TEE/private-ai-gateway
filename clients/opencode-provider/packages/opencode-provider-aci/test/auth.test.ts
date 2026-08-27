import { expect, test } from "bun:test";

import { createDeviceAuthMethod } from "../src/index.ts";

test("maps a device authorization grant into an OpenCode API credential", async () => {
  const responses = [
    Response.json({
      device_code: "device-secret",
      user_code: "ABCD-EFGH",
      verification_uri: "https://cloud.example/cli/verify",
      verification_uri_complete: "https://cloud.example/cli/verify?code=ABCD-EFGH",
      expires_in: 30,
      interval: 0.001,
    }),
    Response.json({ access_token: "llm-key", expires_in: null, redpill_key_id: 42 }),
  ];
  const method = createDeviceAuthMethod({
    label: "Cloud account",
    baseURL: "https://cloud.example",
    clientId: "opencode",
    scope: "redpill:api-key",
    fetch: async () => {
      const response = responses.shift();
      if (!response) throw new Error("unexpected request");
      return response;
    },
  });
  if (method.type !== "oauth") throw new Error("expected an OAuth method");

  const authorization = await method.authorize();
  if (authorization.method !== "auto") throw new Error("expected automatic device login");
  expect(authorization.url).toContain("ABCD-EFGH");
  expect(await authorization.callback()).toEqual({
    type: "success",
    key: "llm-key",
    metadata: { keyId: "42" },
  });
});
