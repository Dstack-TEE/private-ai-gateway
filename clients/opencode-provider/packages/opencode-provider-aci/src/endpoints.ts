/**
 * The AI SDK package OpenCode loads for ACI providers. The `aisdk:` prefix
 * selects OpenCode's AI SDK runtime, which is what exposes `ctx.aisdk.hook`
 * and lets the plugin replace the provider's transport with the verified
 * ACI fetch.
 */
export const OPENCODE_ACI_PACKAGE = "aisdk:@ai-sdk/openai-compatible";

/** AI SDK package name after OpenCode strips the `aisdk:` prefix. */
export const AISDK_OPENAI_COMPATIBLE = "@ai-sdk/openai-compatible";

/** Compare HTTP(S) endpoints without a trailing-slash difference. */
export function sameEndpoint(left: string, right: string): boolean {
  try {
    const a = new URL(left);
    const b = new URL(right);
    const path = (url: URL) => url.pathname.replace(/\/+$/, "");
    return a.origin === b.origin && path(a) === path(b);
  } catch {
    return false;
  }
}

/** Reject requests that do not target the verified gateway origin. */
export function verifiedEndpointOnly(
  request: RequestInfo | URL,
  verifiedBaseURL: string,
): string | undefined {
  const target =
    typeof request === "string" ? request : request instanceof URL ? request.href : request.url;
  try {
    const origin = new URL(target).origin;
    const verified = new URL(verifiedBaseURL).origin;
    return origin === verified ? undefined : `${origin} is not the verified gateway (${verified})`;
  } catch {
    return "invalid ACI request URL";
  }
}

/**
 * OpenCode adds a non-object `provider` field to request bodies when a custom
 * provider entry exists in configuration. ACI serving constraints require that
 * field to be a JSON object when present, so drop OpenCode's own provider id
 * while leaving a caller-supplied routing object untouched.
 */
export function sanitizeAciRequestBody(body: unknown): unknown {
  if (typeof body !== "string") return body;
  let value: unknown;
  try {
    value = JSON.parse(body);
  } catch {
    return body;
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) return body;
  const root = value as Record<string, unknown>;
  const provider = root.provider;
  if (
    provider === undefined ||
    (typeof provider === "object" && provider !== null && !Array.isArray(provider))
  ) {
    return body;
  }
  delete root.provider;
  return JSON.stringify(root);
}
