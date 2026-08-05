// Attested TLS SPKI pinning.
//
// Replaces the per-field E2EE request encryption: the gateway's public TLS
// key is listed in its attestation report (`workload_keyset.tls_public_keys`)
// and cryptographically bound to the attested workload. By pinning that SPKI
// for the configured base host we get the same end-to-end property — request
// and response are readable only by the attested workload — because the TLS
// session is keyed by the private half of the attested key.
//
// Implementation is deliberately thin so it stays out of pi's transport:
//   - one shared `EnvHttpProxyAgent` (honors HTTP(S)_PROXY like pi's own
//     dispatcher) whose `connect.checkServerIdentity` fails closed unless the
//     peer SPKI matches the pin registered for that host;
//   - a single wrapper around `globalThis.fetch` that injects that dispatcher
//     per-request ONLY for pinned hosts and delegates everything else to the
//     underlying fetch (pi's undici 8 fetch + its global dispatcher are left
//     untouched for all other traffic).
//
// Pins are supplied per session from a fresh, validated attestation report
// (see index.ts). `checkServerIdentity` reads the live pin map, so a key
// rotation is applied on the next request without recreating the dispatcher.

import { EnvHttpProxyAgent } from "undici";
import { LOG_PREFIX } from "./constants.ts";
import crypto from "node:crypto";

/** host(lowercase) -> attested SPKI SHA-256 hex (lowercase). */
const pins = new Map<string, string>();

let fetchWrapped = false;
let baseFetch: typeof globalThis.fetch | undefined;

function computeSpkiSha256Hex(der: Uint8Array): string {
  const x509 = new crypto.X509Certificate(der);
  const spki = x509.publicKey.export({ type: "spki", format: "der" }) as Buffer;
  return crypto.createHash("sha256").update(spki).digest("hex");
}

function hexEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length || a.length === 0) return false;
  return crypto.timingSafeEqual(Buffer.from(a, "hex"), Buffer.from(b, "hex"));
}

function normalizeHost(host: string): string {
  return host.trim().toLowerCase();
}

/** Extract the target hostname from fetch's first argument, if possible. */
function hostOfInput(input: RequestInfo | URL): string | undefined {
  try {
    if (typeof input === "string") return normalizeHost(new URL(input).hostname);
    if (input instanceof URL) return normalizeHost(input.hostname);
    const url = (input as Request).url;
    return url ? normalizeHost(new URL(url).hostname) : undefined;
  } catch {
    return undefined;
  }
}

/** TLS callback; returns undefined to accept, or an Error to reject. */
function checkServerIdentity(
  hostname: string,
  cert: { raw: Uint8Array },
): Error | undefined {
  const expected = pins.get(normalizeHost(hostname));
  if (!expected) return undefined; // no pin configured → default TLS validation
  let actual: string;
  try {
    actual = computeSpkiSha256Hex(cert.raw);
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
}

let pinnedDispatcher: ReturnType<typeof createPinnedDispatcher> | undefined;
let rejectUnauthorized = true;
let ca: string | undefined;

/** @internal Test-only hook: relax peer validation (rejectUnauthorized /
 *  extra CA certs) so the pin logic can be exercised against a local TLS
 *  server with a locally-signed cert. Production defaults to full CA
 *  validation on top of the pin. */
export function setPinningRejectUnauthorizedForTests(flag: boolean): void {
  rejectUnauthorized = flag;
  pinnedDispatcher = undefined;
}

/** @internal Test-only hook: trust `ca` (PEM) when connecting, so a local
 *  CA-signed test server reaches the peer-certificate check that the pin
 *  validates. */
export function setPinningCaForTests(caPem: string | undefined): void {
  ca = caPem;
  pinnedDispatcher = undefined;
}

function createPinnedDispatcher() {
  return new EnvHttpProxyAgent({
    allowH2: false,
    connect: { checkServerIdentity, rejectUnauthorized, ...(ca ? { ca } : {}) },
  });
}

function getPinnedDispatcher() {
  if (!pinnedDispatcher) pinnedDispatcher = createPinnedDispatcher();
  return pinnedDispatcher;
}

/** Register the attested SPKI pin for a host. Idempotent. */
export function setPin(host: string, spkiSha256Hex: string): void {
  pins.set(normalizeHost(host), spkiSha256Hex.toLowerCase());
}

/** Remove the pin for a host. */
export function clearPin(host: string): void {
  pins.delete(normalizeHost(host));
}

/** Remove all registered pins. */
export function clearPins(): void {
  pins.clear();
}

/** Current pin for a host, or undefined. */
export function getPin(host: string): string | undefined {
  return pins.get(normalizeHost(host));
}

/** Whether a fetch wrapper is currently installed. */
export function isFetchPinningInstalled(): boolean {
  return fetchWrapped;
}

/**
 * Install the global fetch wrapper once. For hosts with a registered pin the
 * request is sent through the pinned dispatcher (fail-closed on SPKI
 * mismatch); every other request delegates unchanged to the underlying fetch,
 * so pi's own dispatcher (proxy, timeouts) and all other providers are
 * unaffected.
 */
export function installFetchPinning(): void {
  if (fetchWrapped) return;
  fetchWrapped = true;
  baseFetch = globalThis.fetch;
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const host = hostOfInput(input);
    if (host && pins.has(host)) {
      return baseFetch!(input, { ...init, dispatcher: getPinnedDispatcher() } as RequestInit);
    }
    return baseFetch!(input, init);
  };
}

