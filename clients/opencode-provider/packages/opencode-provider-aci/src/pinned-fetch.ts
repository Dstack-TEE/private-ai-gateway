// Attested TLS SPKI pinning + exchange capture for opencode.
//
// Where the pi provider wraps globalThis.fetch with an undici dispatcher, the
// opencode provider injects a custom `fetch` into the provider options of
// @ai-sdk/openai-compatible. That single injection point gives us everything
// pi had to reach for events/global wrappers to approximate:
//   - TLS pinning: opencode plugins run inside opencode's Bun runtime, and
//     Bun's fetch accepts `tls: { checkServerIdentity }` per request, so the
//     attested SPKI pin is enforced per connection for the gateway host (fail
//     closed on mismatch). The pin comes from a fresh, validated attestation
//     report (workload_keyset.tls_public_keys, see index.ts).
//   - Receipt headers: x-receipt-id / x-aci-identity / x-aci-keyset-digest are
//     read straight off the Response, no provider-response event needed.
//   - Body-hash bytes: the request body and the streamed response bytes are
//     captured as they pass through, so receipts can be FULLY verified
//     (signature AND body hashes). The pi provider cannot see response bytes
//     and caps its footer at "verified*"; here "verified" is honest.
//
// Fail-closed posture: when pinning is enabled and no pin is established (and
// the user did not opt into failOpenOnUnpinned), inference traffic to the
// gateway host is rejected before it leaves the process rather than silently
// downgrading to plain CA-TLS. ACI bootstrap endpoints (/v1/aci/*) are exempt
// so a fresh attestation can always be fetched to install or refresh a pin.

import crypto from "node:crypto";

import { LOG_PREFIX } from "./constants.ts";

/** Minimal shape of the TLS peer certificate Bun/Node hand to
 *  checkServerIdentity. `pubkey` is the DER SubjectPublicKeyInfo. */
export interface PeerCertificateLike {
  pubkey?: Uint8Array;
  raw?: Uint8Array;
}

/** SHA-256 (lowercase hex) of the certificate's SPKI, matching how the
 *  gateway computes `tls_public_keys[].spki_sha256` in its attestation.
 *
 *  IMPORTANT: `cert.pubkey` is NOT the SPKI DER — in both Node and Bun it is
 *  the public key BIT STRING contents (no AlgorithmIdentifier wrapper), so
 *  hashing it never matches the attested SPKI. The SPKI must be derived from
 *  the full certificate (`cert.raw`) via X509 export, exactly as the pi
 *  provider does. `pubkey` is only a last-resort fallback for runtimes whose
 *  PeerCertificate carries no raw DER. */
export function computeSpkiSha256Hex(cert: PeerCertificateLike): string {
  if (cert.raw && cert.raw.length > 0) {
    const x509 = new crypto.X509Certificate(cert.raw);
    const spki = x509.publicKey.export({ type: "spki", format: "der" }) as Buffer;
    return crypto.createHash("sha256").update(spki).digest("hex");
  }
  if (cert.pubkey && cert.pubkey.length > 0) {
    return crypto.createHash("sha256").update(cert.pubkey).digest("hex");
  }
  throw new Error("peer certificate carries neither raw DER nor pubkey");
}

function hexEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length || a.length === 0) return false;
  return crypto.timingSafeEqual(Buffer.from(a, "hex"), Buffer.from(b, "hex"));
}

function normalizeHost(host: string): string {
  return host.trim().toLowerCase();
}

/** Target hostname from fetch's first argument, if possible. */
export function hostOfInput(input: RequestInfo | URL): string | undefined {
  try {
    if (typeof input === "string") return normalizeHost(new URL(input).hostname);
    if (input instanceof URL) return normalizeHost(input.hostname);
    const url = (input as Request).url;
    return url ? normalizeHost(new URL(url).hostname) : undefined;
  } catch {
    return undefined;
  }
}

function pathnameOfInput(input: RequestInfo | URL): string {
  try {
    return new URL(typeof input === "string" ? input : ((input as Request).url ?? String(input)))
      .pathname;
  } catch {
    return "";
  }
}

/** True when the request targets a model-inference path (not an ACI bootstrap
 *  endpoint like /v1/aci/attestation). Unparseable URLs are treated as
 *  inference: stay strict. */
export function isInferencePath(input: RequestInfo | URL): boolean {
  const path = pathnameOfInput(input);
  if (!path) return true;
  return !path.startsWith("/v1/aci/");
}

/** Per-host attested SPKI pins. The callback shape matches Bun's
 *  `fetch(init.tls.checkServerIdentity)` and node:tls. */
export class TlsPinManager {
  private pins = new Map<string, string>();

  /** Register the attested SPKI pin for a host. Idempotent. */
  setPin(host: string, spkiSha256Hex: string): void {
    this.pins.set(normalizeHost(host), spkiSha256Hex.toLowerCase());
  }

  /** Remove the pin for a host. */
  clearPin(host: string): void {
    this.pins.delete(normalizeHost(host));
  }

  /** Current pin for a host, or undefined. */
  getPin(host: string): string | undefined {
    return this.pins.get(normalizeHost(host));
  }

  /** TLS callback; returns undefined to accept, or an Error to reject. Hosts
   *  without a pin get default TLS validation (the callback is only attached
   *  for pinned hosts, so this is defense in depth). */
  checkServerIdentity = (hostname: string, cert: PeerCertificateLike): Error | undefined => {
    // node:tls reports "host:port" when a custom port is in play; the pin is
    // registered under the bare hostname from the configured base URL.
    const bare = normalizeHost(hostname).split(":")[0];
    const expected = this.pins.get(bare);
    if (!expected) return undefined;
    let actual: string;
    try {
      actual = computeSpkiSha256Hex(cert);
    } catch (error) {
      return new Error(
        `${LOG_PREFIX} could not compute peer SPKI for ${hostname}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
    if (hexEqualHex(actual, expected)) return undefined;
    return new Error(
      `${LOG_PREFIX} TLS SPKI pin mismatch for ${hostname}: peer=${actual} expected=${expected}`,
    );
  };
}

/** Terminal SSE marker: `data: [DONE]`. The gateway can append keepalive
 *  padding (blank lines) after the [DONE] frame, so completion is "bytes,
 *  ignoring trailing newlines, end with the marker" — NOT an exact tail
 *  match. */
const SSE_DONE_TEXT = new TextEncoder().encode("data: [DONE]");
const LF = 0x0a;

/** Non-null finish_reason marks the semantic end of a chat stream. Consumers
 *  (the AI SDK) may stop reading right after this frame and cancel — the
 *  [DONE] frame never enters the tee — so this is an alternative completion
 *  trigger. Matched against a rolling text tail (the finish frame can be
 *  split across TCP chunks). */
const SSE_FINISH_RE =
  /"finish_reason"\s*:\s*"(stop|length|tool_calls|content_filter|function_call)"/;

/** True when the accumulated stream bytes end with the terminal SSE [DONE]
 *  frame, tolerating any amount of trailing newline padding. Walks backwards
 *  across chunk boundaries without joining the buffers. */
function hasSseDoneTerminator(chunks: Uint8Array[]): boolean {
  let ci = chunks.length - 1;
  let offset = ci >= 0 ? chunks[ci].length - 1 : -1;
  const advance = (): boolean => {
    while (offset < 0) {
      ci--;
      if (ci < 0) return false;
      offset = chunks[ci].length - 1;
    }
    return true;
  };
  // Skip trailing newline padding.
  while (true) {
    if (!advance()) return false;
    if (chunks[ci][offset] !== LF) break;
    offset--;
  }
  // Match the marker backwards.
  for (let m = SSE_DONE_TEXT.length - 1; m >= 0; m--) {
    if (!advance()) return false;
    if (chunks[ci][offset] !== SSE_DONE_TEXT[m]) return false;
    offset--;
  }
  return true;
}

export interface CapturedExchange {
  url: string;
  status: number;
  ok: boolean;
  headers: Record<string, string>;
  /** Exact request body bytes we sent, when captureable (string/Buffer JSON). */
  requestBody?: Uint8Array;
  /** Exact response bytes the model streamed back. Present ONLY when the
   *  exchange closed at true EOF (complete bytes, safe for body-hash
   *  verification). Absent on SSE [DONE] completion: the gateway can append
   *  keepalive padding AFTER the [DONE] frame and its
   *  response.returned.body_hash commits to those bytes too, so truncated
   *  bytes must never be hash-checked. */
  responseBytes?: Uint8Array;
  /** "eof": stream read to completion (responseBytes present).
   *  "sse-done": terminal [DONE] frame seen but consumer did not read to EOF
   *  (responseBytes absent — signature/request-hash level only). */
  completion: "eof" | "sse-done";
}

export interface AciFetchDeps {
  manager: TlsPinManager;
  /** Whether the request host is the configured gateway host. */
  isGatewayHost(host: string): boolean;
  /** Live config reads (config can change between requests). */
  pinningEnabled(): boolean;
  failOpenOnUnpinned(): boolean;
  /** Resolve a fresh attestation and install the pin for the host. Returns
   *  true when a pin is established. Implementations must dedupe concurrent
   *  calls and may serve a cached attestation while fresh. */
  ensurePinned(host: string): Promise<boolean>;
  /** Called per gateway exchange with captured headers/bytes. May fire twice
   *  for one SSE exchange: once on the terminal [DONE] frame (completion
   *  "sse-done", no responseBytes) and again if the consumer then reads to
   *  EOF (completion "eof", full responseBytes — an upgrade). */
  onExchange?(exchange: CapturedExchange): void;
  /** Underlying fetch (defaults to globalThis.fetch). */
  baseFetch?: typeof fetch;
}

function headersToRecord(headers: Headers): Record<string, string> {
  const out: Record<string, string> = {};
  headers.forEach((value, key) => {
    out[key.toLowerCase()] = value;
  });
  return out;
}

/** Exact bytes of a fetch request body, when it is a synchronous body type
 *  (the AI SDK sends JSON strings). Streams are not captured. */
function bodyBytes(body: unknown): Uint8Array | undefined {
  if (typeof body === "string") return new TextEncoder().encode(body);
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  return undefined;
}

/**
 * Create the fetch injected into the provider's options. Traffic to hosts
 * other than the configured gateway delegates to the underlying fetch
 * untouched; gateway traffic is pinned/captured as described above.
 */
export function createAciFetch(deps: AciFetchDeps): typeof fetch {
  const baseFetch = deps.baseFetch ?? globalThis.fetch;
  const { manager } = deps;

  return async function aciFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    const host = hostOfInput(input);
    if (!host || !deps.isGatewayHost(host)) {
      return baseFetch(input, init);
    }

    const inference = isInferencePath(input);
    let pinned = manager.getPin(host) !== undefined;

    if (deps.pinningEnabled() && inference && !pinned) {
      // Lazy pin install on first inference use: resolve the attested SPKI
      // from a fresh, validated attestation before the request can leave.
      pinned = await deps.ensurePinned(host);
    }

    if (deps.pinningEnabled() && inference && !pinned && !deps.failOpenOnUnpinned()) {
      throw new Error(
        `${LOG_PREFIX} host ${host} requires an attested TLS pin but none is established; ` +
          `blocked to avoid a cleartext downgrade. Check the attestation/logs, or set ` +
          `failOpenOnUnpinned to run unpinned with a warning.`,
      );
    }

    const requestBody = bodyBytes(init?.body);
    const pinnedNow = manager.getPin(host) !== undefined;

    // Bun-specific init: `tls.checkServerIdentity` enforces the pin per
    // connection. Unknown to standard RequestInit; cast deliberately.
    const effectiveInit = pinnedNow
      ? ({
          ...init,
          tls: { checkServerIdentity: manager.checkServerIdentity, rejectUnauthorized: true },
        } as unknown as RequestInit)
      : init;

    const response = await baseFetch(input, effectiveInit);
    const headers = headersToRecord(response.headers);

    if (!response.body) {
      deps.onExchange?.({
        url: pathnameOfInput(input),
        status: response.status,
        ok: response.ok,
        headers,
        requestBody,
        completion: "eof",
      });
      return response;
    }

    // Tee the response stream: pass chunks through untouched while collecting
    // the exact bytes. Completion semantics (see CapturedExchange):
    //   - SSE streams: consumers (AI SDK) typically CANCEL the body right
    //     after the terminal `data: [DONE]` frame instead of reading to EOF.
    //     Fire "sse-done" there (no responseBytes — the gateway's
    //     response.returned.body_hash covers post-[DONE] keepalive padding
    //     we may never see). If the consumer then reads on to EOF, fire
    //     "eof" with the complete bytes (an upgrade: full body-hash check).
    //   - Non-streaming responses: read to EOF, fires "eof" only.
    //   - Any other cancel (mid-stream abort): nothing fires, so a partial
    //     stream can never produce a hash "mismatch".
    const chunks: Uint8Array[] = [];
    let byteLength = 0;
    let sawSseDone = false;
    let sawEof = false;
    const requestUrl = pathnameOfInput(input);
    // Rolling decoded tail for finish_reason detection (JSON is ASCII; a
    // streaming decoder keeps multibyte chars intact across chunks).
    const tailDecoder = new TextDecoder();
    let tailText = "";
    const fire = (completion: "eof" | "sse-done", withBytes: boolean) => {
      const bytes = withBytes ? new Uint8Array(byteLength) : undefined;
      if (bytes) {
        let offset = 0;
        for (const chunk of chunks) {
          bytes.set(chunk, offset);
          offset += chunk.length;
        }
      }
      deps.onExchange?.({
        url: requestUrl,
        status: response.status,
        ok: response.ok,
        headers,
        requestBody,
        responseBytes: bytes,
        completion,
      });
    };
    const tee = new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        chunks.push(chunk);
        byteLength += chunk.length;
        tailText = (tailText + tailDecoder.decode(chunk, { stream: true })).slice(-512);
        controller.enqueue(chunk);
        if (!sawSseDone && (hasSseDoneTerminator(chunks) || SSE_FINISH_RE.test(tailText))) {
          sawSseDone = true;
          fire("sse-done", false);
        }
      },
      flush() {
        if (sawEof) return;
        sawEof = true;
        fire("eof", true);
      },
    });

    const body = response.body.pipeThrough(tee);
    return new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}
