import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { once } from "node:events";
import { access, constants, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [directory] = process.argv.slice(2);
assert.ok(directory && path.isAbsolute(directory), "Supply the installed binary directory");
const binary = (name) => path.join(directory, `${name}${process.platform === "win32" ? ".exe" : ""}`);
for (const name of ["pag", "pag-service", "pap", "private-ai-gateway-helper"]) {
  await access(binary(name), process.platform === "win32" ? constants.F_OK : constants.X_OK);
}
const home = await mkdtemp(path.join(os.tmpdir(), "tauri-sidecars-"));
const env = { ...process.env, PRIVATE_AI_GATEWAY_HOME: home, ACI_API_KEY: "", NO_PROXY: "127.0.0.1,localhost", no_proxy: "127.0.0.1,localhost" };
const requests = [];
// A deliberately unavailable loopback service: no quote verification can fetch
// collateral, and none of the commands may progress to inference or sessions.
const server = createServer((request, response) => {
  requests.push(new URL(request.url, "http://localhost").pathname);
  response.writeHead(503, { "Content-Type": "application/json" });
  response.end(JSON.stringify({ error: "local fixture unavailable" }));
});

function run(name, args, expected) {
  return new Promise((resolve, reject) => {
    execFile(binary(name), args, { env, timeout: 20_000, maxBuffer: 1_048_576 }, (error, stdout, stderr) => {
      try {
        assert.equal(error?.code ?? 0, expected, `${name}: unexpected exit (${stderr})`);
        resolve(stdout);
      } catch (failure) {
        reject(failure);
      }
    });
  });
}

try {
  const fixture = fileURLToPath(new URL("../../../tests/fixtures/aci_report_fixture.json", import.meta.url));
  const nonce = "cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d";
  const audit = JSON.parse(await run("pap", ["audit", "--report", fixture, "--nonce", nonce, "--json"], 1));
  assert.equal(audit.verdict.verified, false);
  assert.ok(audit.checks.some((check) => check.id === "id-1" && check.status !== "pass"));
  assert.ok(audit.checks.some((check) => check.id === "id-2" && check.status === "pass"));
  const malformed = path.join(home, "malformed.json");
  await writeFile(malformed, "{", { mode: 0o600 });
  await run("pap", ["audit", "--report", malformed, "--json"], 1);

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const url = `http://127.0.0.1:${address.port}`;
  for (const command of ["verify", "sessions", "send"]) {
    await run("pap", [command, url, "--json"], 1);
  }
  const events = (await run("pap", ["serve", url, "--json-events", "--listen", "127.0.0.1:0", "--control", "127.0.0.1:0"], 1))
    .trim().split(/\r?\n/).map((line) => JSON.parse(line));
  assert.ok(events.some((event) => event.type === "fatal"));
  assert.ok(events.every((event) => event.type !== "ready"));
  assert.deepEqual(requests, Array(4).fill("/v1/aci/attestation"));
  console.log("Installed ACI: offline audit, malformed input, verify/sessions/send/serve fail-closed checks passed");

  const tokens = path.join(home, ".private-ai-gateway", "agent-tokens");
  await mkdir(tokens, { recursive: true, mode: 0o700 });
  for (const agent of ["codex", "claude-code", "opencode", "pi", "hermes", "openclaw", "oh-my-pi"]) {
    await run("private-ai-gateway-helper", ["--agent-token", agent], 1);
    // Synthetic local token fixture, not a provider credential or a claim that
    // a verified agent connection has been established.
    const token = `smoke-local-token-${agent}`;
    const tokenPath = path.join(tokens, agent);
    await writeFile(tokenPath, token, { mode: 0o600, flag: "wx" });
    assert.equal(await run("private-ai-gateway-helper", ["--agent-token", agent], 0), token);
    assert.equal(await readFile(tokenPath, "utf8"), token);
    await rm(tokenPath);
    await run("private-ai-gateway-helper", ["--agent-token", agent], 1);
  }
  await run("private-ai-gateway-helper", ["--agent-token", "unknown-agent"], 1);
  console.log("Installed helper: seven isolated token reads and missing/deleted/unknown-agent failures passed");
} finally {
  server.closeAllConnections();
  await new Promise((resolve, reject) => server.close((error) => {
    if (error && error.code !== "ERR_SERVER_NOT_RUNNING") reject(error);
    else resolve();
  }));
  await rm(home, { recursive: true, force: true });
}
