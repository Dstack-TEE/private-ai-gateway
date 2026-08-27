import {
  checkSessionApiVersion,
  checkSessionEvidence,
  computeSessionId,
  type SessionRecord,
} from "@phala/aci-verifier";

export interface AciSessionCheck {
  name: "content-address" | "api-version" | "validity-window" | "evidence";
  ok: boolean;
  detail: string;
}

export interface AciSessionAudit {
  sessionId: string;
  session: SessionRecord;
  verified: boolean;
  checks: readonly AciSessionCheck[];
}

export function isAciSessionId(value: string): boolean {
  return /^[0-9a-f]{64}$/.test(value);
}

function sessionRecord(value: unknown): SessionRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("session endpoint returned an invalid document");
  }
  const record = value as Record<string, unknown>;
  for (const field of ["api_version", "upstream_name", "verifier_id"] as const) {
    if (typeof record[field] !== "string" || record[field].length === 0) {
      throw new TypeError(`session document has an invalid ${field}`);
    }
  }
  if (
    record.endpoint !== undefined &&
    record.endpoint !== null &&
    typeof record.endpoint !== "string"
  ) {
    throw new TypeError("session document has an invalid endpoint");
  }
  for (const field of ["established_at", "expires_at"] as const) {
    const seconds = record[field];
    if (
      typeof seconds !== "number" ||
      !Number.isSafeInteger(seconds) ||
      seconds < 0 ||
      Number.isNaN(new Date(seconds * 1000).getTime())
    ) {
      throw new TypeError(`session document has an invalid ${field}`);
    }
  }
  if (!Array.isArray(record.channel_binding)) {
    throw new TypeError("session document has an invalid channel_binding");
  }
  if (!("claims" in record)) {
    throw new TypeError("session document has no claims");
  }
  if (!record.evidence || typeof record.evidence !== "object" || Array.isArray(record.evidence)) {
    throw new TypeError("session document has invalid evidence");
  }
  return record as SessionRecord;
}

export async function auditAciSession(sessionId: string, value: unknown): Promise<AciSessionAudit> {
  if (!isAciSessionId(sessionId)) {
    throw new TypeError("session id must be a 64-character lowercase hex digest");
  }
  const session = sessionRecord(value);
  const computedId = await computeSessionId(session);
  const checks: AciSessionCheck[] = [
    {
      name: "content-address",
      ok: computedId === sessionId,
      detail:
        computedId === sessionId ? sessionId : `expected ${sessionId}, computed ${computedId}`,
    },
    {
      name: "api-version",
      ok: checkSessionApiVersion(session),
      detail: String(session.api_version ?? "missing"),
    },
    {
      name: "validity-window",
      ok: session.established_at <= session.expires_at,
      detail: `${session.established_at}-${session.expires_at}`,
    },
    {
      name: "evidence",
      ok: await checkSessionEvidence(session.evidence),
      detail: String(session.evidence?.digest ?? "missing"),
    },
  ];
  return {
    sessionId,
    session,
    verified: checks.every((check) => check.ok),
    checks,
  };
}
