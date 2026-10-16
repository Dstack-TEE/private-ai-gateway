#!/usr/bin/env node
/**
 * check-models.mjs — end-to-end health check for the pi-provider-phala-cloud
 * (or any ACI-branded pi provider) model catalog.
 *
 * What it does:
 *   1. Fetches the gateway /v1/models catalog (reasoning/is_tee/max-tokens view).
 *   2. Sends one real `pi -p` request per model x thinking level through the
 *      pi extension (the true integration surface), classifies every failure:
 *        - ok                    request verified end-to-end (ACI receipt passed)
 *        - aci-receipt           model answered but receipt verification failed
 *                                (usually empty upstream session evidence)
 *        - role-developer        upstream rejects the `developer` message role
 *        - effort-none           upstream rejects reasoning effort "none" (off level)
 *        - effort-vocab          upstream rejects the thinking-level vocabulary
 *        - max-tokens-metadata   catalog max_output_length exceeds upstream limit
 *        - rate-limit            transient 429 (auto-retried once)
 *        - timeout / unknown
 *   3. With --deep: for aci-receipt failures, pulls the cited upstream session
 *      and reports the route (chutes: vs phala-<n>:) and whether evidence is empty.
 *
 * Usage:
 *   node scripts/check-models.mjs [--package ../packages/pi-provider-phala-cloud]
 *     [--base-url https://inference.phala.com/v1] [--provider phala]
 *     [--filter substr] [--levels off,low,medium,high] [--jobs 4]
 *     [--per-run-timeout 150] [--deep] [--report out.md] [--json out.json]
 *
 * Auth: PHALA_AI_API_KEY env, else ~/.pi/agent/auth.json[<provider>].key
 */
import { createAciProvider } from "@phala/aci-provider";
import { discoverAciModelCatalog } from "@phala/aci-provider";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

function arg(name, dflt) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : dflt;
}
const FLAG = (n) => process.argv.includes(`--${n}`);

const OPTS = {
  package: resolve(here, arg("package", "../packages/pi-provider-phala-cloud")),
  baseUrl: arg("base-url", "https://inference.phala.com/v1"),
  provider: arg("provider", "phala"),
  filter: arg("filter", ""),
  levels: arg("levels", "off,low,medium,high").split(",").map((s) => s.trim()),
  levelsNonReasoning: arg("levels-plain", "off,low").split(",").map((s) => s.trim()),
  jobs: Number(arg("jobs", "4")),
  perRunTimeoutSec: Number(arg("per-run-timeout", "150")),
  deep: FLAG("deep"),
  report: arg("report", ""),
  json: arg("json", ""),
  prompt: arg("prompt", "Reply with exactly: OK"),
  samples: Number(arg("samples", "2")),
};

function resolveApiKey() {
  if (process.env.PHALA_AI_API_KEY) return process.env.PHALA_AI_API_KEY;
  try {
    const auth = JSON.parse(readFileSync(`${homedir()}/.pi/agent/auth.json`, "utf8"));
    const key = auth[OPTS.provider]?.key ?? auth.phala?.key;
    if (key) return key;
  } catch {}
  throw new Error(
    "no API key: set PHALA_AI_API_KEY or ensure ~/.pi/agent/auth.json has a key for this provider",
  );
}

const API_KEY = resolveApiKey();
const authedFetch = (input, init = {}) => {
  const headers = new Headers(init.headers ?? {});
  headers.set("authorization", `Bearer ${API_KEY}`);
  return fetch(input, { ...init, headers });
};

// ---------------------------------------------------------------- catalog
async function fetchCatalog() {
  const { models, raw } = await discoverAciModelCatalog({
    config: { baseURL: OPTS.baseUrl, models: { isTeeOnly: false }, trust: {}, receipts: { verification: "response", historySize: 1 } },
    fetch: authedFetch,
    timeoutMs: 30_000,
  });
  const byId = new Map(models.map((m) => [m.id, m]));
  const rows = raw
    .filter((r) => typeof r.id === "string")
    .map((r) => {
      const m = byId.get(r.id);
      return {
        id: r.id,
        isTee: r.is_tee === true,
        reasoning: m?.reasoning ?? false,
        contextWindow: m?.contextWindow ?? null,
        maxOutputTokens: m?.maxOutputTokens ?? null,
      };
    })
    .filter((r) => (OPTS.filter ? r.id.includes(OPTS.filter) : true))
    .sort((a, b) => a.id.localeCompare(b.id));
  return rows;
}

// ---------------------------------------------------------------- classify
function classify(exitCode, log) {
  const t = log.replace(/\s+/g, " ");
  if (exitCode === 0) {
    // Heuristic: reasoning text leaked into the content at a non-reasoning level.
    const leaked = /<\/think>|The user (wants|is asking)/.test(t) && !t.trimEnd().endsWith("OK");
    return { kind: "ok", detail: leaked ? "ok (reasoning text leaked at this level)" : "ok" };
  }
  if (/receipt verification failed/.test(t)) {
    const m = t.match(/NOT VERIFIED \((\d+) fail: ([^;)]+)/);
    return { kind: "aci-receipt", detail: m ? `receipt: ${m[2]} fail` : "receipt verification failed" };
  }
  if (/429|Rate limit exceeded/.test(t)) return { kind: "rate-limit", detail: t.slice(0, 160) };
  if (/Unexpected message role/.test(t)) return { kind: "role-developer", detail: "400 Unexpected message role (developer role rejected)" };
  if (/Unexpected reasoning effort|Harmony does not support/.test(t)) {
    return { kind: "effort-vocab", detail: t.match(/400: .{0,140}/)?.[0] ?? "reasoning effort rejected" };
  }
  if (/max_tokens=?\d*.*(max_model_len|cannot be greater than)/.test(t)) {
    return { kind: "max-tokens-metadata", detail: t.match(/400: .{0,160}/)?.[0] ?? "max_tokens exceeds upstream limit" };
  }
  if (/The request was rejected as invalid/.test(t)) {
    return { kind: "generic-400", detail: "400 The request was rejected as invalid (role or effort; use --deep/manual probe)" };
  }
  if (/404|no route available/.test(t)) return { kind: "no-route", detail: t.slice(0, 160) };
  if (/timed out|TimeoutError/.test(t)) return { kind: "timeout", detail: t.slice(0, 160) };
  return { kind: "unknown", detail: t.slice(-160) };
}

function runPi(args, timeoutMs) {
  // pi waits on stdin even in -p mode when it is a pipe: end it immediately.
  return new Promise((resolvePromise) => {
    const p = spawn("pi", args, { cwd: "/tmp", stdio: ["pipe", "pipe", "pipe"], env: { ...process.env } });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => p.kill("SIGTERM"), timeoutMs);
    p.stdout.on("data", (d) => (stdout += d));
    p.stderr.on("data", (d) => (stderr += d));
    p.stdin.end();
    p.on("error", (err) => {
      clearTimeout(timer);
      resolvePromise({ code: 127, log: String(err) });
    });
    p.on("close", (code, signal) => {
      clearTimeout(timer);
      resolvePromise({ code: code ?? (signal ? 124 : 1), log: `${stdout}\n${stderr}${signal ? `\n(signal ${signal})` : ""}` });
    });
  });
}

async function piProbe(modelId, level) {
  const args = [
    "-ne", "-e", OPTS.package,
    "--provider", OPTS.provider,
    "--model", `${OPTS.provider}/${modelId}`,
    "--thinking", level,
    "-nt", "--no-session",
    "-p", OPTS.prompt,
  ];
  const { code, log } = await runPi(args, OPTS.perRunTimeoutSec * 1000);
  return classify(code, log);
}

// ------------------------------------------------------- deep receipt audit
async function deepAudit(modelId) {
  const provider = createAciProvider({
    baseURL: OPTS.baseUrl,
    models: { isTeeOnly: true },
    trust: {},
    receipts: { verification: "response", historySize: OPTS.samples + 2 },
  });
  await provider.connect();
  const conn = provider["connection"];
  const out = [];
  try {
    for (let i = 0; i < OPTS.samples; i++) {
      let status = "ok";
      try {
        const res = await provider.fetch(`${OPTS.baseUrl}/chat/completions`, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${API_KEY}` },
          body: JSON.stringify({
            model: modelId,
            messages: [{ role: "user", content: "hi" }],
            max_tokens: 16,
            reasoning_effort: "low",
            stream: false,
          }),
        });
        await res.text();
      } catch (e) {
        status = e.message.slice(0, 120);
      }
      const rs = conn.receipts();
      const last = rs[rs.length - 1];
      if (!last) { out.push({ status, route: null, note: "no receipt" }); continue; }
      const rres = await provider.fetch(`${OPTS.baseUrl}/aci/receipts/${last.receiptId ?? last.id}`, {
        headers: { Accept: "application/json", authorization: `Bearer ${API_KEY}` },
      });
      const doc = await rres.json();
      const ev = doc.event_log ?? [];
      const route = ev.find((e2) => e2.type === "route.selected")?.target_route_id ?? null;
      const uv = ev.find((e2) => e2.type === "upstream.verified");
      let evidence = null;
      if (uv?.session_id) {
        const sres = await provider.fetch(`${OPTS.baseUrl}/aci/sessions/${uv.session_id}`, {
          headers: { Accept: "application/json", authorization: `Bearer ${API_KEY}` },
        });
        if (sres.status === 200) {
          const sdoc = await sres.json();
          const s = sdoc.session ?? sdoc;
          evidence = s.evidence && Object.keys(s.evidence).length > 0 ? "present" : "EMPTY";
        } else evidence = `fetch ${sres.status}`;
      }
      out.push({ status, route, upstreamModel: uv?.model_id ?? null, evidence });
    }
  } finally {
    provider.close();
  }
  return out;
}

// ------------------------------------------------------------------- main
const catalog = await fetchCatalog();
console.log(`catalog: ${catalog.length} models from ${OPTS.baseUrl}`);
const jobs = [];
for (const m of catalog) {
  const levels = m.reasoning ? OPTS.levels : OPTS.levelsNonReasoning;
  for (const lv of levels) jobs.push({ id: m.id, reasoning: m.reasoning, level: lv });
}
console.log(`matrix: ${jobs.length} runs (${OPTS.jobs} parallel, ${OPTS.perRunTimeoutSec}s per run)\n`);

const results = [];
let cursor = 0;
async function worker() {
  while (cursor < jobs.length) {
    const j = jobs[cursor++];
    const r = await piProbe(j.id, j.level);
    if (r.kind === "rate-limit") {
      await new Promise((r2) => setTimeout(r2, 5000));
      const retry = await piProbe(j.id, j.level);
      results.push({ ...j, ...retry, retried: true });
      console.log(`${retry.kind === "ok" ? "✓" : "✗"} ${j.id} @${j.level} (retry after 429) → ${retry.kind}`);
    } else {
      results.push({ ...j, ...r });
      console.log(`${r.kind === "ok" ? "✓" : "✗"} ${j.id} @${j.level} → ${r.kind}`);
    }
  }
}
await Promise.all(Array.from({ length: OPTS.jobs }, worker));

// Per-model rollup
const byModel = new Map();
for (const r of results) {
  if (!byModel.has(r.id)) byModel.set(r.id, []);
  byModel.get(r.id).push(r);
}

console.log("\n================ per-model summary ================");
for (const [id, rs] of [...byModel.entries()].sort()) {
  const kinds = new Set(rs.map((r) => r.kind));
  const verdict =
    kinds.size === 1 && kinds.has("ok")
      ? "PASS"
      : rs.every((r) => r.kind === "aci-receipt")
        ? "ACI-RECEIPT-FAIL"
        : kinds.has("aci-receipt")
          ? "MIXED (receipt + api)"
          : "API-FAIL";
  console.log(`${verdict.padEnd(22)} ${id}  [${rs.map((r) => `${r.level}:${r.kind}`).join(", ")}]`);
}

// Deep audit of aci-receipt models
let audits = {};
const receiptModels = [...byModel.entries()].filter(([, rs]) => rs.some((r) => r.kind === "aci-receipt")).map(([id]) => id);
if (OPTS.deep && receiptModels.length > 0) {
  console.log("\n================ deep receipt audit ================");
  for (const id of receiptModels) {
    try {
      audits[id] = await deepAudit(id);
      for (const a of audits[id]) {
        console.log(`${id}: route=${a.route} upstream=${a.upstreamModel} evidence=${a.evidence} status=${a.status}`);
      }
    } catch (e) {
      audits[id] = { error: e.message };
      console.log(`${id}: audit error: ${e.message}`);
    }
  }
}

// ------------------------------------------------------------- reports
const summary = {
  baseUrl: OPTS.baseUrl,
  provider: OPTS.provider,
  generatedAt: new Date().toISOString(),
  catalog,
  results,
  audits,
};
if (OPTS.json) {
  writeFileSync(OPTS.json, JSON.stringify(summary, null, 2));
  console.log(`\njson → ${OPTS.json}`);
}

const lines = [];
lines.push(`# Model check — ${OPTS.provider} @ ${OPTS.baseUrl}`, "");
lines.push(`Generated: ${summary.generatedAt}. Runs: ${results.length}.`, "");
lines.push("| model | verdict | per-level |", "|---|---|---|");
for (const [id, rs] of [...byModel.entries()].sort()) {
  const kinds = new Set(rs.map((r) => r.kind));
  const verdict =
    kinds.size === 1 && kinds.has("ok")
      ? "✅ PASS"
      : rs.every((r) => r.kind === "aci-receipt")
        ? "❌ ACI-RECEIPT"
        : kinds.has("aci-receipt")
          ? "❌ MIXED"
          : "❌ API";
  lines.push(`| ${id} | ${verdict} | ${rs.map((r) => `${r.level}: ${r.kind}`).join("<br>")} |`);
}
lines.push("", "## Failure details", "");
for (const r of results.filter((r2) => r2.kind !== "ok")) {
  lines.push(`- **${r.id}** @\`${r.level}\` — **${r.kind}**: ${r.detail}${r.retried ? " (after 429 retry)" : ""}`);
}
if (Object.keys(audits).length) {
  lines.push("", "## Receipt audits (route / evidence)", "");
  for (const [id, a] of Object.entries(audits)) {
    lines.push(`- **${id}**: ${(Array.isArray(a) ? a : [a]).map((x) => `route=${x.route ?? x.error} evidence=${x.evidence ?? "-"}`).join(" ; ")}`);
  }
}
if (OPTS.report) {
  writeFileSync(OPTS.report, lines.join("\n") + "\n");
  console.log(`report → ${OPTS.report}`);
}
