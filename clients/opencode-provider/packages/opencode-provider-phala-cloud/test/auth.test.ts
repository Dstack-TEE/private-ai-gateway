import { expect, test } from "bun:test";
import { createPhalaCloudAccountAuth } from "@phala/aci-provider/phala-cloud";
import { createOpenCodeAccountAuthMethod } from "@phala/opencode-provider-aci";

test("maps the shared Phala account flow into OpenCode native auth", async () => {
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
    Response.json({
      user: { username: "alice" },
      workspace: { name: "Confidential AI", slug: "confidential-ai" },
    }),
  ];
  const method = createOpenCodeAccountAuthMethod(
    createPhalaCloudAccountAuth({
      baseURL: "https://cloud.example",
      clientId: "opencode",
      fetch: async () => {
        const response = responses.shift();
        if (!response) throw new Error("unexpected request");
        return response;
      },
    }),
  );
  if (method.type !== "oauth") throw new Error("expected an OAuth method");

  const authorization = await method.authorize();
  if (authorization.method !== "auto") throw new Error("expected automatic device login");
  expect(authorization.url).toContain("ABCD-EFGH");
  expect(await authorization.callback()).toEqual({
    type: "success",
    key: "llm-key",
    metadata: {
      keyId: "42",
      username: "alice",
      workspaceName: "Confidential AI",
      workspaceSlug: "confidential-ai",
    },
  });
});
