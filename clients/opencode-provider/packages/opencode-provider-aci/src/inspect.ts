import { tool, type ToolDefinition } from "@opencode-ai/plugin";
import type { AciProvider, AciReceiptAudit, AciSessionAudit } from "@phala/aci-provider";

function isoTime(value: number): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "invalid" : date.toISOString();
}

function keySummary(keys: readonly { key_id?: unknown; algo?: unknown }[]): string {
  return keys.length === 0
    ? "none"
    : keys.map((key) => `${String(key.key_id)} (${String(key.algo)})`).join(", ");
}

function receiptEventLines(receipt: AciReceiptAudit["receipt"]): string[] {
  if (!Array.isArray(receipt.event_log)) return [];
  const interesting = new Set([
    "upstream.verified",
    "request.received",
    "request.forwarded",
    "response.received",
    "response.returned",
  ]);
  const lines: string[] = [];
  for (const value of receipt.event_log) {
    if (!value || typeof value !== "object" || Array.isArray(value)) continue;
    const event = value as Record<string, unknown>;
    const type = typeof event.type === "string" ? event.type : "";
    if (!interesting.has(type)) continue;
    if (type === "upstream.verified") {
      const fields = ["result", "required", "provider", "model_id", "session_id"]
        .filter((field) => event[field] !== undefined)
        .map((field) => `${field}=${String(event[field])}`);
      lines.push(`${type} ${fields.join(" ")}`.trim());
    } else if (event.body_hash !== undefined) {
      lines.push(`${type} body_hash=${String(event.body_hash)}`);
    }
  }
  return lines;
}

function providerOrThrow(getProvider: () => AciProvider | undefined): AciProvider {
  const provider = getProvider();
  if (!provider) throw new Error("ACI provider is not connected to a verified gateway");
  return provider;
}

function attestationSummary(provider: AciProvider): string {
  const status = provider.status();
  const identity = status.identity;
  if (!identity) throw new Error("verified ACI identity is unavailable");
  const keyset = identity.keyset;
  const releasePolicy = provider.config.trust.acceptedComposeHashes?.length
    ? "reviewed release accepted"
    : "measurement verified, release not pinned";
  return [
    `Provider: ${status.phase}`,
    `Origin: ${identity.origin}`,
    `API version: ${String(identity.report.api_version)}`,
    `Compose hash: ${identity.composeHash}`,
    `Release policy: ${releasePolicy}`,
    "Report binding: verified",
    `Keyset digest: ${identity.workloadKeysetDigest}`,
    `TLS SPKI pins: ${identity.tlsSpkiPins.join(", ")}`,
    `Keyset not_after: ${isoTime(keyset.not_after * 1000)}`,
    `Encryption keys (${keyset.e2ee_public_keys.length}): ${keySummary(keyset.e2ee_public_keys)}`,
    `Receipt signing keys (${keyset.receipt_signing_keys.length}): ${keySummary(keyset.receipt_signing_keys)}`,
    `Verified at: ${isoTime(identity.verifiedAt)}`,
    `Expires at: ${isoTime(identity.expiresAt)}`,
  ].join("\n");
}

function receiptSummary(audit: AciReceiptAudit): string {
  const receipt = audit.receipt;
  return [
    `Receipt: ${audit.receiptId}`,
    `API version: ${String(receipt.api_version)}`,
    `Signing key: ${String(receipt.key_id)}`,
    `Model: ${String(receipt.model)}`,
    `Endpoint: ${String(receipt.endpoint)}`,
    `Served at: ${typeof receipt.served_at === "number" ? isoTime(receipt.served_at * 1000) : "invalid"}`,
    `Keyset digest: ${String(receipt.workload_keyset_digest)}`,
    `Status: ${audit.exchange.status}`,
    `Recorded at: ${isoTime(audit.exchange.recordedAt)}`,
    ...receiptEventLines(receipt),
    `Verdict: ${audit.transcript.verdict.line}`,
    ...audit.transcript.lines.map(
      (line) => `${line.status.toUpperCase()} ${line.id}${line.detail ? `: ${line.detail}` : ""}`,
    ),
  ].join("\n");
}

function sessionSummary(audit: AciSessionAudit): string {
  const session = audit.session;
  return [
    `Session: ${audit.sessionId}`,
    `API version: ${session.api_version}`,
    `Upstream: ${String(session.upstream_name)}`,
    `Endpoint: ${String(session.endpoint ?? "none")}`,
    `Verifier: ${String(session.verifier_id)}`,
    `Established: ${isoTime(Number(session.established_at) * 1000)}`,
    `Expires: ${isoTime(Number(session.expires_at) * 1000)}`,
    ...audit.checks.map((check) => `${check.ok ? "PASS" : "FAIL"} ${check.name}: ${check.detail}`),
  ].join("\n");
}

export function createAciInspectTool(getProvider: () => AciProvider | undefined): ToolDefinition {
  return tool({
    description:
      "Inspect the local ACI verified connection, attestation, receipt history, or an attested session. This is read-only and returns verification metadata, never prompts or responses.",
    args: {
      action: tool.schema
        .enum(["status", "attestation", "receipts", "receipt", "session"])
        .describe("The ACI information to inspect"),
      id: tool.schema
        .string()
        .optional()
        .describe("Receipt id for receipt, or the required 64-hex session id for session"),
    },
    async execute({ action, id }, context) {
      const provider = providerOrThrow(getProvider);
      if (action === "status") {
        const status = provider.status();
        return [
          `Phase: ${status.phase}`,
          `Models: ${status.models.length}`,
          `Receipts retained: ${status.receipts.length}`,
          ...(status.error ? [`Error: ${status.error}`] : []),
        ].join("\n");
      }
      if (action === "attestation") return attestationSummary(provider);
      if (action === "receipts") {
        const receipts = provider.receipts();
        if (receipts.length === 0) return "No ACI receipts have been recorded in this process.";
        return receipts
          .map(
            (receipt) =>
              `${receipt.receiptId} ${receipt.method} ${receipt.path} HTTP ${receipt.status} ${receipt.responseComplete ? "complete" : "streaming"}`,
          )
          .join("\n");
      }
      if (action === "receipt") return receiptSummary(await provider.verifyReceipt(id));
      if (!id) throw new Error("session inspection requires a session id");
      return sessionSummary(await provider.verifySession(id, { signal: context.abort }));
    },
  });
}
