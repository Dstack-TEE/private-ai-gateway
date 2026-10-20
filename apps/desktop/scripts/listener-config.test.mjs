import assert from "node:assert/strict";
import { test } from "node:test";
import { listenerDraft, listenerPort } from "../src/renderer/lib/local-api-config.ts";

const draft = (fields) => ({ ...listenerDraft({ listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 }), ...fields });

test("the port saves as the number typed, within the listener's range", () => {
  assert.equal(listenerPort(draft({}), 1024), 4180);
  assert.equal(listenerPort(draft({ port: "1024" }), 1024), 1024);
  assert.equal(listenerPort(draft({ port: "65535" }), 1024), 65535);
  assert.equal(listenerPort(draft({ port: "1" }), 1), 1);
  // Spellings a number field accepts for the same integer.
  for (const port of ["004180", "4180.0", "4.18e3", "4180e0"]) assert.equal(listenerPort(draft({ port }), 1024), 4180, port);
  assert.equal(listenerPort(draft({ port: "1e4" }), 1024), 10000);
});

test("a cleared, fractional or out-of-range port is refused, never saved as 0", () => {
  for (const port of ["", " ", "0", "1023", "65536", "4180.5", "1e3", "1e5", "-1", "99999999"]) {
    assert.throws(() => listenerPort(draft({ port }), 1024), { message: "Enter a port from 1024 to 65535." }, JSON.stringify(port));
  }
  for (const port of ["", "\t", "0", "0.5", "65536"]) {
    assert.throws(() => listenerPort(draft({ port }), 1), { message: "Enter a port from 1 to 65535." }, JSON.stringify(port));
  }
});

test("the address must be an IP address, and an all-interface listener needs a client host", () => {
  assert.throws(() => listenerPort(draft({ listenAddress: "" }), 1), { message: "Enter a valid IPv4 or IPv6 listen address." });
  assert.throws(() => listenerPort(draft({ listenAddress: "localhost" }), 1), { message: "Enter a valid IPv4 or IPv6 listen address." });
  for (const listenAddress of ["0.0.0.0", "::"]) {
    assert.throws(() => listenerPort(draft({ listenAddress, clientHost: " " }), 1), { message: "Enter a client host for an all-interface listener." });
    assert.equal(listenerPort(draft({ listenAddress, clientHost: "192.168.1.5" }), 1), 4180);
  }
});
