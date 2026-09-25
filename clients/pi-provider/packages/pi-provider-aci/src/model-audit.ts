// Registration-time audit of upstream attestation sessions (ACI §8.1/§9.2).
//
// The catalog contract (§5: "Clients MUST NOT infer trust from /v1/models
// entries") says nothing about whether a model's upstream channel can be
// deep-audited. The sessions list endpoint does: its abbreviated entries keep
// `evidence.digest` while dropping the bulky `evidence.data`. A session whose
// evidence digest is absent cannot satisfy §9.2 check 2, so every response
// served through it fails receipt verification — the model is usable in name
// only. Querying the list per model at registration time lets the extension
// surface that before the first prompt instead of mid-turn.

/** Audit outcome for one catalog model. */
export type ModelAuditStatus = "auditable" | "unauditable" | "unknown";

/**
 * Classify one `/aci/sessions?model=` payload.
 *
 * - "unauditable": at least one current session lacks `evidence.digest` —
 *   receipt verification against any of them fails §9.2 check 2 fail-closed.
 * - "auditable": every listed session carries an evidence digest.
 * - "unknown": the list is empty or unreadable (a direct service publishes no
 *   sessions, which is fine; an aggregator with no verified upstream is not —
 *   the distinction needs the service report, so stay neutral here).
 */
export function classifySessionList(payload: unknown): ModelAuditStatus {
  const sessions = (payload as { sessions?: unknown } | null)?.sessions;
  if (!Array.isArray(sessions) || sessions.length === 0) return "unknown";
  for (const session of sessions) {
    const digest = (session as { evidence?: { digest?: unknown } } | null)?.evidence?.digest;
    if (typeof digest !== "string" || digest.length === 0) return "unauditable";
  }
  return "auditable";
}

export interface AuditCatalogModelsOptions {
  baseUrl: string;
  fetch: typeof globalThis.fetch;
  modelIds: readonly string[];
  /** Max parallel session-list requests. */
  concurrency?: number;
  /** Per-request timeout; a timed-out or failed model is "unknown". */
  timeoutMs?: number;
}

/**
 * Query the sessions list for every catalog model. Never rejects: audit
 * failures degrade to "unknown" so a flaky transparency endpoint cannot
 * break model registration.
 */
export async function auditCatalogModels(
  options: AuditCatalogModelsOptions,
): Promise<Map<string, ModelAuditStatus>> {
  const concurrency = options.concurrency ?? 4;
  const timeoutMs = options.timeoutMs ?? 5_000;
  const results = new Map<string, ModelAuditStatus>();
  let cursor = 0;
  const modelIds = [...options.modelIds];

  async function worker(): Promise<void> {
    while (cursor < modelIds.length) {
      const id = modelIds[cursor++];
      try {
        const url = `${options.baseUrl.replace(/\/$/, "")}/aci/sessions?model=${encodeURIComponent(id)}`;
        const response = await options.fetch(url, {
          headers: { Accept: "application/json" },
          signal: AbortSignal.timeout(timeoutMs),
        });
        if (!response.ok) {
          results.set(id, "unknown");
          continue;
        }
        results.set(id, classifySessionList(await response.json()));
      } catch {
        results.set(id, "unknown");
      }
    }
  }

  await Promise.all(Array.from({ length: Math.max(1, concurrency) }, worker));
  return results;
}

/** User-facing explanation shared by the model-select warning and the
 * pre-prompt stream guard. */
export function unauditableModelMessage(providerLabel: string, modelId: string): string {
  return (
    `${providerLabel}: model "${modelId}" is routed through an upstream whose ` +
    "attestation evidence is currently missing from its ACI session records " +
    "(a gateway-side issue, not a model or client problem). Responses served " +
    "through it fail receipt verification and will be blocked. Pick another " +
    "model, or ask the gateway operator to fix upstream evidence persistence."
  );
}
