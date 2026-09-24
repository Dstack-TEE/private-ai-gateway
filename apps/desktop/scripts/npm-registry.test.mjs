import assert from "node:assert/strict";
import test from "node:test";

import { registryVersionState, waitForRegistry } from "./npm-registry.mjs";

const registry = "https://registry.example";
const platform = { name: "@phala/private-ai-proxy-linux-x64", version: "1.2.3-beta.4", integrity: "sha512-platform" };
const wrapper = { name: "private-ai-proxy", version: "1.2.3-beta.4", integrity: "sha512-wrapper" };

function key({ name, version }) {
  return `${name}@${version}`;
}

function json(status, body) {
  return { status, ok: status >= 200 && status < 300, json: async () => body };
}

// Serves a registry whose version documents and install packument can lag
// independently, as the public CDN does right after a publish. Documents and
// packument entries are keyed by `name@version`.
function fakeRegistry({ documents = {}, packument = {} }) {
  const requests = [];
  const fetchImpl = async (url, { headers }) => {
    requests.push({ url, accept: headers.accept });
    const [name, version, extra] = url.slice(`${registry}/`.length).split("/");
    assert.equal(extra, undefined, `scoped names must be escaped in ${url}`);
    const packageName = decodeURIComponent(name);
    if (version === undefined) {
      assert.equal(headers.accept, "application/vnd.npm.install-v1+json");
      return json(200, { versions: Object.fromEntries(Object.entries(packument)
        .filter(([spec]) => spec.startsWith(`${packageName}@`))
        .map(([spec, integrity]) => [spec.slice(packageName.length + 1), { dist: { integrity } }])) });
    }
    const response = documents[`${packageName}@${version}`];
    if (response instanceof Error) throw response;
    if (typeof response === "number") return json(response, {});
    return response ? json(200, { version, dist: { integrity: response } }) : json(404, {});
  };
  return { fetchImpl, requests };
}

function clock() {
  let current = 0;
  return {
    now: () => current,
    sleep: async (milliseconds) => { current += milliseconds; },
  };
}

test("reports existing versions and rejects changed contents", async () => {
  const { fetchImpl } = fakeRegistry({ documents: { [key(wrapper)]: wrapper.integrity } });
  assert.deepEqual(await registryVersionState(wrapper, { registry, fetchImpl }), { state: "published" });
  assert.deepEqual(await registryVersionState(platform, { registry, fetchImpl }), { state: "missing" });
  await assert.rejects(
    registryVersionState({ ...wrapper, integrity: "sha512-other" }, { registry, fetchImpl }),
    /exists in the version document with different contents/,
  );
  const failing = fakeRegistry({ documents: { [key(wrapper)]: 503 } });
  assert.equal((await registryVersionState(wrapper, { registry, fetchImpl: failing.fetchImpl })).state, "unavailable");
});

test("waits until every version document and the install packument serve the versions", async () => {
  const documents = {};
  const packument = {};
  const { fetchImpl } = fakeRegistry({ documents, packument });
  const time = clock();
  let polls = 0;
  await waitForRegistry([platform, wrapper], {
    registry,
    fetchImpl,
    intervalMs: 1000,
    timeoutMs: 10_000,
    ...time,
    sleep: async (milliseconds) => {
      polls += 1;
      // The version documents appear first, then transiently fail, and the
      // install packument catches up last.
      if (polls === 1) Object.assign(documents, { [key(platform)]: platform.integrity, [key(wrapper)]: new Error("reset") });
      if (polls === 2) documents[key(wrapper)] = wrapper.integrity;
      if (polls === 3) packument[key(platform)] = platform.integrity;
      if (polls === 4) packument[key(wrapper)] = wrapper.integrity;
      await time.sleep(milliseconds);
    },
    log: () => {},
  });
  assert.equal(polls, 4);
});

test("fails once the bounded wait expires", async () => {
  const { fetchImpl, requests } = fakeRegistry({ documents: { [key(platform)]: platform.integrity } });
  await assert.rejects(
    waitForRegistry([platform, wrapper], { registry, fetchImpl, intervalMs: 1000, timeoutMs: 3000, ...clock(), log: () => {} }),
    /Timed out waiting for https:\/\/registry\.example to serve @phala\/private-ai-proxy-linux-x64@1\.2\.3-beta\.4 \(not yet in the install packument\), private-ai-proxy@1\.2\.3-beta\.4 \(missing\)/,
  );
  assert.ok(requests.some(({ url }) => url === `${registry}/@phala%2fprivate-ai-proxy-linux-x64/1.2.3-beta.4`));
});

test("stops waiting when the install packument has different contents", async () => {
  const { fetchImpl } = fakeRegistry({
    documents: { [key(platform)]: platform.integrity },
    packument: { [key(platform)]: "sha512-other" },
  });
  await assert.rejects(
    waitForRegistry([platform], { registry, fetchImpl, ...clock(), log: () => {} }),
    /exists in the install packument with different contents/,
  );
});
