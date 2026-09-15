#!/usr/bin/env node
import { spawn } from "node:child_process";
import { once } from "node:events";
import { writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";

const [aciPath, remoteUrl, reportPath] = process.argv.slice(2);
if (!aciPath || !remoteUrl || !reportPath) {
  throw new Error("Usage: live-sidecar-smoke.mjs <aci-path> <remote-url> <report-path>");
}

const child = spawn(aciPath, [
  "serve",
  remoteUrl,
  "--listen",
  "127.0.0.1:0",
  "--control",
  "127.0.0.1:0",
  "--json-events",
], {
  shell: false,
  stdio: ["ignore", "pipe", "pipe"],
});
const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
let stderr = "";
child.stderr.setEncoding("utf8");
child.stderr.on("data", (chunk) => {
  stderr = `${stderr}${chunk}`.slice(-4_096);
});

const ready = new Promise((resolve, reject) => {
  lines.on("line", (line) => {
    try {
      const event = JSON.parse(line);
      if (event.type === "ready") {
        resolve(event);
      } else if (event.type === "fatal") {
        reject(new Error(event.message ?? "ACI emitted a fatal event"));
      }
    } catch {
      reject(new Error("ACI emitted invalid JSON event data"));
    }
  });
  child.once("error", reject);
  child.once("exit", (code) => {
    reject(new Error(`ACI exited before ready with status ${code ?? "unknown"}: ${stderr.trim()}`));
  });
});
const timeout = new Promise((_, reject) => {
  setTimeout(() => reject(new Error("Timed out waiting for live ACI verification")), 180_000).unref();
});

try {
  const event = await Promise.race([ready, timeout]);
  if (!isRecord(event) || typeof event.proxy_url !== "string") {
    throw new Error("ACI ready event did not include a proxy URL");
  }
  const response = await fetch(`${event.proxy_url}/v1/models`, {
    signal: AbortSignal.timeout(30_000),
  });
  const body = await response.json();
  if (!response.ok || !isRecord(body) || !Array.isArray(body.data) || body.data.length === 0) {
    throw new Error(`Live /v1/models smoke failed with HTTP ${response.status}`);
  }
  await writeFile(reportPath, JSON.stringify({
    remoteUrl: event.remote_url,
    proxyUrl: event.proxy_url,
    teeType: event.tee_type,
    trustLevel: event.trust_level,
    keysetDigest: event.keyset_digest,
    checks: isRecord(event.verification) && Array.isArray(event.verification.checks)
      ? event.verification.checks.length
      : 0,
    models: body.data.length,
  }, null, 2));
} finally {
  lines.close();
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await Promise.race([
      once(child, "exit"),
      new Promise((resolve) => setTimeout(resolve, 3_000)),
    ]);
  }
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
