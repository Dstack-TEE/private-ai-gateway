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
