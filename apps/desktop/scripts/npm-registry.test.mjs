import assert from "node:assert/strict";
import test from "node:test";

import { registryVersionState } from "./npm-registry.mjs";

const registry = "https://registry.example";
const wrapper = { name: "private-ai-proxy", version: "1.2.3-beta.4", integrity: "sha512-wrapper" };

function json(status, body) {
  return { status, ok: status >= 200 && status < 300, json: async () => body };
}

function fakeRegistry(documents) {
  return async (url) => {
    const version = decodeURIComponent(url.slice(`${registry}/private-ai-proxy/`.length));
    const response = documents[version];
    if (response instanceof Error) throw response;
    if (typeof response === "number") return json(response, {});
    return response ? json(200, { version, dist: { integrity: response } }) : json(404, {});
  };
}

test("reports existing versions and rejects changed contents", async () => {
  const fetchImpl = fakeRegistry({ [wrapper.version]: wrapper.integrity });
  assert.deepEqual(await registryVersionState(wrapper, { registry, fetchImpl }), { state: "published" });
  assert.deepEqual(
    await registryVersionState({ ...wrapper, version: "1.2.3-beta.4-linux-x64" }, { registry, fetchImpl }),
    { state: "missing" },
  );
  await assert.rejects(
    registryVersionState({ ...wrapper, integrity: "sha512-other" }, { registry, fetchImpl }),
    /exists in the registry with different contents/,
  );
});

test("reports registry failures as unavailable instead of missing", async () => {
  for (const response of [503, new Error("reset")]) {
    const fetchImpl = fakeRegistry({ [wrapper.version]: response });
    assert.equal((await registryVersionState(wrapper, { registry, fetchImpl })).state, "unavailable");
  }
});
