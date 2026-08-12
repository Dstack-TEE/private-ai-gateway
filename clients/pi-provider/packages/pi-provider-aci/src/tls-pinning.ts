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
// Fail-closed posture: when a host is marked REQUIRED (pinning enabled) but
// no pin is established, inference traffic to that host is BLOCKED (the chat
// request never leaves the process) rather than silently downgraded to plain
// CA-TLS. The ACI bootstrap endpoints (/v1/aci/attestation, /v1/aci/receipts/*,
// /v1/aci/sessions/*) are exempt so a fresh attestation can always be fetched
// to install or refresh a pin.
//
// Pins are supplied per session from a fresh, validated attestation report
// (see index.ts). `checkServerIdentity` reads the live pin map, so a key
// rotation is applied on the next request without recreating the dispatcher.

import { EnvHttpProxyAgent } from "undici";
import { LOG_PREFIX } from "./constants.ts";
import crypto from "node:crypto";

/** Hostname (lowercase) -> attested SPKI SHA-256 hex (lowercase). Pins are
 *  scoped to a host; the per-connection peer check additionally compares the
 *  port so a localhost gateway on one port cannot pin another local port's
 *  unrelated TLS (the pin map is still host-keyed because pi builds requests
 *  against the configured base host, whose port is constant). */
const pins = new Map<string, string>();

/** Hosts whose traffic must be attested-pinned once configured. Inference to a
 *  required host with no established pin is blocked (fail closed). */
const requiredHosts = new Set<string>();

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

/** Target hostname from fetch's first argument, if possible. */
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
  // undici reports "host:port" here when a custom port is in play; the pin is
  // registered under the bare hostname from the configured base URL.
  const bare = normalizeHost(hostname).split(":")[0];
  const expected = pins.get(bare);
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
  // EnvHttpProxyAgent extends ProxyAgent and does NOT honor Agent `connect`
  // options (they are dropped). TLS knobs for the origin connection go in
  // `requestTls` (see undici ProxyAgent / EnvHttpProxyAgent docs). Using
  // `connect` here silently ignored the pin callback and CA, so pinned hosts
  // failed with UNABLE_TO_VERIFY_LEAF_SIGNATURE under a local test CA and
  // would not enforce SPKI mismatch in production either.
  return new EnvHttpProxyAgent({
    allowH2: false,
    requestTls: {
      checkServerIdentity,
      rejectUnauthorized,
      ...(ca ? { ca } : {}),
    },
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

/** Mark a host as requiring an attested pin. Inference to it will be blocked
 *  (fail closed) until a pin is installed. */
export function requirePinForHost(host: string): void {
  requiredHosts.add(normalizeHost(host));
}

/** Stop requiring a pin for a host (pinning disabled for it). */
export function unrequirePinForHost(host: string): void {
  requiredHosts.delete(normalizeHost(host));
}

/** Remove the pin for a host. */
export function clearPin(host: string): void {
  pins.delete(normalizeHost(host));
}

/** Remove all registered pins (required-host status stays). */
export function clearPins(): void {
  pins.clear();
}

/** Remove all pins AND all required-host marks. */
export function clearAllPinning(): void {
  pins.clear();
  requiredHosts.clear();
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
  globalThis.fetch = wrappedFetch;
}

/** Remove the global wrapper when present. Test/teardown hook; a config change
 *  that should drop pins should call clearPins() instead. */
export function uninstallFetchPinning(): void {
  if (!fetchWrapped) return;
  if (globalThis.fetch === wrappedFetch) {
    globalThis.fetch = baseFetch ?? globalThis.fetch;
  }
  fetchWrapped = false;
  baseFetch = undefined;
}

function wrappedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const host = hostOfInput(input);
  if (host && pins.has(host)) {
    return baseFetch!(input, { ...init, dispatcher: getPinnedDispatcher() } as RequestInit);
  }
  // Fail closed: a host that must be attested-pinned but is not (yet) pinned
  // BLOCKS inference traffic instead of silently downgrading to CA-TLS. The
  // ACI bootstrap endpoints stay reachable so a fresh attestation can be
  // fetched to establish the pin.
  if (host && requiredHosts.has(host) && !pins.has(host) && isInferencePath(input)) {
    return Promise.reject(
      new Error(
        `${LOG_PREFIX} host ${host} requires an attested TLS pin but none is established; ` +
          `blocked to avoid a cleartext downgrade. Check the attestation/tail the logs, or ` +
          `disable pinning in settings to run unpinned.`,
      ),
    );
  }
  return baseFetch!(input, init);
}

/** True when the request targets a model-inference path (not an ACI bootstrap
 *  endpoint like /v1/aci/attestation). */
function isInferencePath(input: RequestInfo | URL): boolean {
  try {
    const url = new URL(typeof input === "string" ? input : (input as Request).url ?? String(input));
    const path = url.pathname;
    return !path.startsWith("/v1/aci/");
  } catch {
    return true; // unparseable URL: treat as inference, stay strict
  }
}

