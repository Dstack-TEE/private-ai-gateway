import assert from "node:assert/strict";
import { test } from "node:test";
import { createDesktopApi } from "./create-api.ts";

test("the API takes the platform's own capabilities and presents a sign-in the backend began", async () => {
  const calls = [];
  const transport = {
    call: async (method, params) => { calls.push([method, params]); return { id: "login-1" }; },
    subscribe: () => () => undefined,
  };
  const platform = { copyText: async () => undefined, closeWindow: undefined, presentAccountLogin: (login) => calls.push(["present", login]) };
  const api = createDesktopApi(transport, platform);
  assert.equal(api.copyText, platform.copyText);
  assert.ok("closeWindow" in api && api.closeWindow === undefined);
  assert.equal("presentAccountLogin" in api, false);
  const login = await api.beginAccountLogin("profile");
  assert.deepEqual(calls, [["begin_account_login", { profile: "profile" }], ["present", login]]);
});
