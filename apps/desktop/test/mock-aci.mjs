#!/usr/bin/env node
import http from "node:http";

const proxy = http.createServer((_request, response) => {
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify({ data: [] }));
});

const receipts = [
  {
    receipt_id: "rcpt-desktop-smoke-0001",
    path: "/v1/chat/completions",
    status: 200,
    streamed: true,
    truncated: false,
    at: Math.floor(Date.now() / 1_000),
    verified: true,
  },
];

const control = http.createServer((request, response) => {
  if (request.method === "GET" && request.url === "/receipts") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(receipts));
    return;
  }
  response.writeHead(404);
  response.end();
});

await Promise.all([
  new Promise((resolve) => proxy.listen(0, "127.0.0.1", resolve)),
  new Promise((resolve) => control.listen(0, "127.0.0.1", resolve)),
]);

const proxyAddress = proxy.address();
const controlAddress = control.address();
if (!proxyAddress || typeof proxyAddress === "string" || !controlAddress || typeof controlAddress === "string") {
  throw new Error("Mock ACI failed to bind local ports");
}

const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
const identity = {
  trust_level: "hardware_verified",
  tee_type: "tdx",
  keyset_digest: `sha256:${"a".repeat(64)}`,
  keyset_not_after: Math.floor(Date.now() / 1_000) + 86_400,
  tls_spki: `sha256:${"b".repeat(64)}`,
  source_provenance: {
    repo_url: "https://github.com/Dstack-TEE/private-ai-gateway",
    repo_commit: "898f19f36978102aade8785632eb36f8d4459337",
    image_digest: `sha256:${"c".repeat(64)}`,
  },
  service_capabilities: {
    serving: "aggregator",
    supported_e2ee_versions: ["2"],
  },
  verification: {
    checks: [
      { id: "id-1", section: "9.1(1)", title: "Hardware quote", status: "pass", detail: "TDX quote verified" },
      { id: "id-2", section: "9.1(2)", title: "Keyset binding", status: "pass", detail: "Keyset digest bound" },
      { id: "id-6", section: "9.1(6)", title: "TLS channel binding", status: "pass", detail: "Observed SPKI matched" },
    ],
    verdict: { verified: true, passed: 3, failed: 0, skipped: 0 },
  },
};
emit({
  ...identity,
  type: "ready",
  schema_version: 1,
  proxy_url: `http://127.0.0.1:${proxyAddress.port}`,
  control_url: `http://127.0.0.1:${controlAddress.port}`,
  remote_url: "https://tee.redpill.ai",
});

if (process.env.MOCK_ACI_RUNTIME_UPDATE === "1") {
  setTimeout(() => emit({
    type: "blocked",
    schema_version: 1,
    reason: "keyset rotation requires re-verification",
  }), 80).unref();
  setTimeout(() => emit({
    ...identity,
    type: "identity_updated",
    schema_version: 1,
    keyset_digest: `sha256:${"d".repeat(64)}`,
  }), 160).unref();
}

setTimeout(() => emit({
  type: "request_complete",
  schema_version: 1,
  method: "POST",
  path: "/v1/chat/completions",
  status: 200,
  streamed: true,
  verified: true,
  detail: "receipt rcpt-desktop-smoke-0001 verified",
}), 250).unref();

const shutdown = () => {
  proxy.close();
  control.close(() => process.exit(0));
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
