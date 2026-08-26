import { createHash, randomBytes } from 'node:crypto';

import { computeVerdict, receiptTranscriptFromDigests, reportTranscript } from '../transcript.js';
import type { AttestationReport, ReceiptEnvelope, SessionRecord, WorkloadKeyset } from '../types.js';
import { createPinnedTransport, type PinnedTransport } from './transport.js';
import {
  AciConnectionError,
  type AciConnection,
  type AciReceiptAudit,
  type ConnectAciOptions,
  type RecordedAciExchange,
  type VerifiedAciIdentity,
} from './types.js';

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_RECEIPT_HISTORY_SIZE = 32;

interface InternalExchange extends RecordedAciExchange {
  requestDigest: string;
  responseDigest?: string;
  completion: Promise<void>;
  identity: VerifiedAciIdentity;
  pinnedSessions: string[];
}

export async function connectAci(options: ConnectAciOptions): Promise<AciConnection> {
  const connection = new NodeAciConnection(options);
  await connection.refresh();
  return connection;
}

class NodeAciConnection implements AciConnection {
  readonly baseURL: string;
  readonly fetch: typeof globalThis.fetch;

  private readonly origin: string;
  private readonly hostname: string;
  private readonly attestationURL: string;
  private readonly options: ConnectAciOptions;
  private currentIdentity?: VerifiedAciIdentity;
  private transport: PinnedTransport | undefined;
  private refreshing: Promise<void> | undefined;
  private readonly exchanges: InternalExchange[] = [];
  private rotationRequired = false;
  private closed = false;

  constructor(options: ConnectAciOptions) {
    validateOptions(options);
    const target = normalizeBaseURL(options.baseURL);
    this.baseURL = target.baseURL;
    this.origin = target.origin;
    this.hostname = target.hostname;
    this.attestationURL = target.attestationURL;
    this.options = options;
    this.fetch = (input, init) => this.secureFetch(input, init);
  }

  get identity(): VerifiedAciIdentity {
    if (!this.currentIdentity) {
      throw new AciConnectionError('closed', 'ACI identity is unavailable');
    }
    return this.currentIdentity;
  }

  receipts(): readonly RecordedAciExchange[] {
    return this.exchanges
      .slice()
      .reverse()
      .map(publicExchange);
  }

  async verifyReceipt(receiptId?: string): Promise<AciReceiptAudit> {
    const exchange = receiptId
      ? [...this.exchanges].reverse().find((entry) => entry.receiptId === receiptId)
      : this.exchanges.at(-1);
    if (!exchange) {
      throw new AciConnectionError(
        'receipt_not_found',
        receiptId
          ? `no recorded ACI exchange cites receipt ${receiptId}`
          : 'no recorded ACI receipt is available',
      );
    }
    const id = exchange.receiptId;
    await exchange.completion;
    await this.ensureFreshIdentity();
    const identity = exchange.identity;
    const receipt = await this.fetchJsonArtifact<ReceiptEnvelope>(`receipts/${encodeURIComponent(id)}`);
    const sessionId = citedSessionId(receipt);
    let session: SessionRecord | undefined;
    if (sessionId) {
      try {
        session = await this.fetchJsonArtifact<SessionRecord>(`sessions/${encodeURIComponent(sessionId)}`);
      } catch (error) {
        if (!(error instanceof AciConnectionError) || error.code !== 'receipt_fetch') throw error;
      }
    }
    const serving = identity.report.service_capabilities?.serving;
    const transcript = await receiptTranscriptFromDigests(
      receipt,
      identity.keyset,
      identity.workloadKeysetDigest,
      {
        request: exchange.requestDigest,
        ...(exchange.responseDigest ? { response: exchange.responseDigest } : {}),
      },
      {
        ...(session === undefined ? {} : { session }),
        ...(exchange.pinnedSessions.length ? { pinnedSessions: exchange.pinnedSessions } : {}),
        requiresVerified:
          this.options.serving?.requireVerified !== false || exchange.pinnedSessions.length > 0,
        ...(typeof serving === 'string' ? { serving } : {}),
      },
    );
    if (exchange.responseError) {
      const responseLine = transcript.lines.find((line) => line.id === 'receipt-4');
      if (responseLine) {
        responseLine.status = 'fail';
        responseLine.detail = `response stream could not be hashed completely: ${exchange.responseError}`;
        delete responseLine.reason;
        transcript.verdict = computeVerdict(transcript.lines);
      }
    }
    return {
      receiptId: id,
      receipt,
      transcript,
      exchange: publicExchange(exchange),
    };
  }

  refresh(): Promise<void> {
    if (this.closed) {
      return Promise.reject(new AciConnectionError('closed', 'ACI connection is closed'));
    }
    if (this.refreshing) return this.refreshing;
    const refresh = this.refreshOnce().finally(() => {
      if (this.refreshing === refresh) this.refreshing = undefined;
    });
    this.refreshing = refresh;
    return refresh;
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    if (this.refreshing) {
      try {
        await this.refreshing;
      } catch {
        // The refresh owns its candidate transport and closes it on failure.
      }
    }
    const transport = this.transport;
    this.transport = undefined;
    if (transport) await transport.close();
  }

  private async refreshOnce(): Promise<void> {
    const identity = await establishIdentity(this.attestationURL, this.origin, this.hostname, this.options);
    const candidate = createPinnedTransport({
      origin: this.origin,
      hostname: this.hostname,
      spkiPins: identity.tlsSpkiPins,
      ...(this.options.proxy === undefined ? {} : { proxy: this.options.proxy }),
      ...(this.options.ca === undefined ? {} : { ca: this.options.ca }),
    });
    try {
      await probePinnedChannel(candidate, this.attestationURL, this.options);
      if (this.closed) throw new AciConnectionError('closed', 'ACI connection closed during refresh');
    } catch (error) {
      await candidate.close();
      if (error instanceof AciConnectionError) throw error;
      throw new AciConnectionError('channel_binding', 'failed to establish the pinned ACI channel', {
        cause: error,
      });
    }

    const previous = this.transport;
    this.transport = candidate;
    this.currentIdentity = identity;
    this.rotationRequired = false;
    if (previous) await previous.close();
  }

  private async secureFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    if (this.closed || !this.transport) {
      throw new AciConnectionError('closed', 'ACI connection is closed');
    }
    await this.ensureFreshIdentity();
    const transport = this.transport;
    if (!transport) throw new AciConnectionError('closed', 'ACI connection is closed');
    const identity = this.identity;
    const prepared = await prepareRequest(input, init, this.options);
    const response = await transport.fetch(prepared.request);
    const observedDigest = response.headers.get('x-aci-keyset-digest');
    if (observedDigest && observedDigest !== identity.workloadKeysetDigest) {
      this.rotationRequired = true;
    }
    const receiptId = response.headers.get('x-receipt-id');
    if (
      prepared.request.method === 'POST' &&
      response.ok &&
      !receiptId &&
      this.options.serving?.requireReceipt !== false
    ) {
      try {
        await response.body?.cancel();
      } catch {
        // The missing receipt is the security failure; cancellation is cleanup.
      }
      throw new AciConnectionError(
        'receipt_missing',
        'successful ACI POST response carries no X-Receipt-Id',
      );
    }
    if (prepared.request.method === 'POST' && receiptId) {
      this.recordExchange(receiptId, prepared, response, identity);
    }
    return response;
  }

  private async ensureFreshIdentity(): Promise<void> {
    if (!this.rotationRequired && Date.now() < this.identity.expiresAt) return;
    try {
      await this.refresh();
    } catch (error) {
      throw new AciConnectionError(
        'identity_expired',
        'ACI workload identity changed or expired and re-verification failed',
        { cause: error },
      );
    }
  }

  private recordExchange(
    receiptId: string,
    prepared: PreparedRequest,
    response: Response,
    identity: VerifiedAciIdentity,
  ): void {
    const entry: InternalExchange = {
      receiptId,
      method: prepared.request.method,
      path: new URL(prepared.request.url).pathname,
      status: response.status,
      recordedAt: Date.now(),
      responseComplete: false,
      requestDigest: prepared.bodyDigest,
      completion: Promise.resolve(),
      identity,
      pinnedSessions: prepared.pinnedSessions,
    };
    entry.completion = digestResponse(response.clone()).then((digest) => {
      entry.responseDigest = digest;
      entry.responseComplete = true;
    }, (error: unknown) => {
      entry.responseError = error instanceof Error ? error.message : String(error);
      entry.responseComplete = true;
    });
    this.exchanges.push(entry);
    const cap = this.options.receiptHistorySize ?? DEFAULT_RECEIPT_HISTORY_SIZE;
    if (this.exchanges.length > cap) this.exchanges.splice(0, this.exchanges.length - cap);
  }

  private async fetchJsonArtifact<T extends object>(path: string): Promise<T> {
    const root = new URL(this.attestationURL);
    root.pathname = root.pathname.replace(/attestation$/, path);
    const response = await this.secureFetch(root, {
      headers: {
        Accept: 'application/json',
        ...(this.options.apiKey ? { Authorization: `Bearer ${this.options.apiKey}` } : {}),
      },
    });
    if (!response.ok) {
      throw new AciConnectionError('receipt_fetch', `ACI artifact fetch returned HTTP ${response.status}`);
    }
    const value: unknown = await response.json().catch((error: unknown) => {
      throw new AciConnectionError('receipt_fetch', 'ACI artifact endpoint returned invalid JSON', {
        cause: error,
      });
    });
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      throw new AciConnectionError('receipt_verification', 'ACI artifact is not a JSON object');
    }
    return value as T;
  }
}

interface PreparedRequest {
  request: Request;
  bodyDigest: string;
  pinnedSessions: string[];
}

async function prepareRequest(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  options: ConnectAciOptions,
): Promise<PreparedRequest> {
  const original = new Request(input, init);
  if (original.method !== 'POST') {
    return { request: original, bodyDigest: await digestRequest(original), pinnedSessions: [] };
  }
  const bytes = new Uint8Array(await original.clone().arrayBuffer());
  const constrained = constrainJsonBody(bytes, options);
  const headers = new Headers(original.headers);
  headers.delete('content-length');
  const request =
    constrained.body === bytes
      ? original
      : new Request(original, { body: Buffer.from(constrained.body), headers });
  return {
    request,
    bodyDigest: digestBytes(constrained.body),
    pinnedSessions: constrained.pinnedSessions,
  };
}

/** @internal Exported for transport-policy conformance tests. */
export function constrainJsonBody(
  body: Uint8Array,
  options: ConnectAciOptions,
): { body: Uint8Array; pinnedSessions: string[] } {
  const fixed = [...(options.serving?.acceptedSessionIds ?? [])];
  const enforceVerified = options.serving?.requireVerified !== false;
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(body));
  } catch {
    return { body, pinnedSessions: [] };
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return { body, pinnedSessions: [] };
  }
  const root = value as Record<string, unknown>;
  const providerValue = root.provider;
  if (
    providerValue !== undefined &&
    (providerValue === null || typeof providerValue !== 'object' || Array.isArray(providerValue))
  ) {
    if (!enforceVerified && fixed.length === 0) return { body, pinnedSessions: [] };
    throw new AciConnectionError(
      'invalid_serving_constraints',
      'provider must be a JSON object when ACI serving constraints are enabled',
    );
  }
  const provider =
    providerValue !== null && typeof providerValue === 'object' && !Array.isArray(providerValue)
      ? { ...(providerValue as Record<string, unknown>) }
      : {};
  const supplied = parseSessionIds(provider.aci_session_ids);
  if (!enforceVerified && fixed.length === 0 && supplied.length === 0) {
    return { body, pinnedSessions: [] };
  }
  const pinnedSessions =
    fixed.length === 0
      ? supplied
      : supplied.length === 0
        ? fixed
        : supplied.filter((id) => fixed.includes(id));
  if (fixed.length > 0 && supplied.length > 0 && pinnedSessions.length === 0) {
    throw new AciConnectionError(
      'invalid_serving_constraints',
      'request session ids do not intersect the connection acceptedSessionIds policy',
    );
  }
  if (pinnedSessions.length > 0) provider.aci_session_ids = pinnedSessions;
  if (enforceVerified || pinnedSessions.length > 0) {
    provider.aci_verified = true;
  }
  root.provider = provider;
  return {
    body: new TextEncoder().encode(JSON.stringify(root)),
    pinnedSessions,
  };
}

function parseSessionIds(value: unknown): string[] {
  if (value === undefined) return [];
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.some((id) => typeof id !== 'string' || !/^[0-9a-f]{64}$/.test(id))
  ) {
    throw new AciConnectionError(
      'invalid_serving_constraints',
      'provider.aci_session_ids must be a non-empty array of lowercase 64-hex ids',
    );
  }
  return [...new Set(value)];
}

async function digestRequest(request: Request): Promise<string> {
  return digestBytes(new Uint8Array(await request.clone().arrayBuffer()));
}

async function digestResponse(response: Response): Promise<string> {
  const hash = createHash('sha256');
  const reader = response.body?.getReader();
  if (reader) {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      hash.update(value);
    }
  }
  return `sha256:${hash.digest('hex')}`;
}

function digestBytes(bytes: Uint8Array): string {
  return `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
}

function publicExchange(exchange: InternalExchange): RecordedAciExchange {
  return {
    receiptId: exchange.receiptId,
    method: exchange.method,
    path: exchange.path,
    status: exchange.status,
    recordedAt: exchange.recordedAt,
    responseComplete: exchange.responseComplete,
    ...(exchange.responseError === undefined ? {} : { responseError: exchange.responseError }),
  };
}

function citedSessionId(receipt: ReceiptEnvelope): string | undefined {
  const events = receipt.event_log;
  if (!Array.isArray(events)) return undefined;
  for (const event of events) {
    if (event === null || typeof event !== 'object' || Array.isArray(event)) continue;
    const fields = event as Record<string, unknown>;
    if (fields.type === 'upstream.verified' && fields.result === 'verified' && typeof fields.session_id === 'string') {
      return fields.session_id;
    }
  }
  return undefined;
}

function validateOptions(options: ConnectAciOptions): void {
  const cap = options.receiptHistorySize ?? DEFAULT_RECEIPT_HISTORY_SIZE;
  if (!Number.isSafeInteger(cap) || cap < 1 || cap > 1_000) {
    throw new AciConnectionError(
      'invalid_policy',
      'receiptHistorySize must be an integer between 1 and 1000',
    );
  }
  for (const [name, value] of [
    ['requireVerified', options.serving?.requireVerified],
    ['requireReceipt', options.serving?.requireReceipt],
  ] as const) {
    if (value !== undefined && typeof value !== 'boolean') {
      throw new AciConnectionError('invalid_policy', `${name} must be a boolean`);
    }
  }
  const composeHashes = options.policy?.acceptedComposeHashes;
  if (composeHashes !== undefined && (!Array.isArray(composeHashes) || composeHashes.length === 0)) {
    throw new AciConnectionError(
      'invalid_policy',
      'acceptedComposeHashes must be a non-empty array when supplied',
    );
  }
  for (const hash of composeHashes ?? []) {
    if (!/^[0-9a-f]{64}$/i.test(hash)) {
      throw new AciConnectionError(
        'invalid_policy',
        'acceptedComposeHashes entries must be 64-character SHA-256 hex digests',
      );
    }
  }
  const sessions = options.serving?.acceptedSessionIds;
  if (sessions !== undefined && (!Array.isArray(sessions) || sessions.length === 0)) {
    throw new AciConnectionError(
      'invalid_policy',
      'acceptedSessionIds must be non-empty when supplied',
    );
  }
  if (options.serving?.requireVerified === false && sessions !== undefined) {
    throw new AciConnectionError(
      'invalid_policy',
      'acceptedSessionIds implies verified serving and cannot be combined with requireVerified=false',
    );
  }
  for (const id of sessions ?? []) {
    if (!/^[0-9a-f]{64}$/.test(id)) {
      throw new AciConnectionError(
        'invalid_policy',
        'acceptedSessionIds entries must be 64-character lowercase hex ids',
      );
    }
  }
}

async function establishIdentity(
  attestationURL: string,
  origin: string,
  hostname: string,
  options: ConnectAciOptions,
): Promise<VerifiedAciIdentity> {
  const nonce = randomBytes(32).toString('hex');
  const report = await fetchAttestation(attestationURL, nonce, options);
  let transcript;
  try {
    transcript = await reportTranscript(report, nonce, {
      online: true,
      ...(options.pccsUrl === undefined ? {} : { pccsUrl: options.pccsUrl }),
      ...(options.policy?.requireProductionOs === undefined
        ? {}
        : { requireProductionOs: options.policy.requireProductionOs }),
      ...(options.policy?.acceptedComposeHashes === undefined
        ? {}
        : { acceptedComposeHashes: options.policy.acceptedComposeHashes }),
    });
  } catch (error) {
    throw new AciConnectionError(
      'attestation_verification',
      `ACI attestation is malformed: ${error instanceof Error ? error.message : String(error)}`,
      { cause: error },
    );
  }

  const required = ['id-1', 'id-2', 'id-3', 'id-4'];
  if (options.policy?.requireProductionOs) required.push('policy-os');
  const failed = required
    .map((id) => ({ id, line: transcript.lines.find((line) => line.id === id) }))
    .filter(({ line }) => line?.status !== 'pass');
  if (failed.length > 0) {
    const detail = failed
      .map(({ id, line }) => `${id}: ${line?.detail ?? 'check did not run'}`)
      .join('; ');
    throw new AciConnectionError(
      'attestation_verification',
      `ACI attestation verification failed: ${detail}`,
    );
  }
  const keyset = transcript.verification.keyset;
  const digest = transcript.verification.workloadKeysetDigest;
  const composeHash = transcript.composeHash;
  if (!keyset || !digest || !composeHash) {
    throw new AciConnectionError(
      'attestation_verification',
      'ACI attestation did not establish a workload identity',
    );
  }
  const pins = tlsPinsForHost(keyset, hostname);
  const verifiedAt = Date.now();
  return {
    origin,
    hostname,
    report,
    keyset,
    workloadKeysetDigest: digest,
    composeHash,
    tlsSpkiPins: pins,
    verifiedAt,
    expiresAt: keyset.not_after * 1000,
    transcript,
  };
}

async function fetchAttestation(
  attestationURL: string,
  nonce: string,
  options: ConnectAciOptions,
): Promise<AttestationReport> {
  const url = new URL(attestationURL);
  url.searchParams.set('nonce', nonce);
  const response = await timedFetch(
    options.bootstrapFetch ?? globalThis.fetch,
    url,
    {
      headers: {
        Accept: 'application/json',
        ...(options.apiKey ? { Authorization: `Bearer ${options.apiKey}` } : {}),
      },
    },
    options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
  );
  if (!response.ok) {
    throw new AciConnectionError(
      'attestation_fetch',
      `ACI attestation endpoint returned HTTP ${response.status}`,
    );
  }
  try {
    return (await response.json()) as AttestationReport;
  } catch (error) {
    throw new AciConnectionError('attestation_fetch', 'ACI attestation endpoint returned invalid JSON', {
      cause: error,
    });
  }
}

async function probePinnedChannel(
  transport: PinnedTransport,
  attestationURL: string,
  options: ConnectAciOptions,
): Promise<void> {
  const url = new URL(attestationURL);
  url.searchParams.set('nonce', randomBytes(32).toString('hex'));
  const response = await timedFetch(
    transport.fetch,
    url,
    {
      headers: {
        Accept: 'application/json',
        ...(options.apiKey ? { Authorization: `Bearer ${options.apiKey}` } : {}),
      },
    },
    options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
  );
  if (!response.ok) {
    throw new AciConnectionError(
      'channel_binding',
      `pinned ACI channel probe returned HTTP ${response.status}`,
    );
  }
  await response.body?.cancel();
}

async function timedFetch(
  fetchImpl: typeof globalThis.fetch,
  input: RequestInfo | URL,
  init: RequestInit,
  timeoutMs: number,
): Promise<Response> {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new AciConnectionError('attestation_fetch', 'timeoutMs must be a positive number');
  }
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  timeout.unref();
  try {
    return await fetchImpl(input, { ...init, signal: controller.signal });
  } catch (error) {
    if (error instanceof AciConnectionError) throw error;
    throw new AciConnectionError(
      'attestation_fetch',
      `ACI request failed: ${error instanceof Error ? error.message : String(error)}`,
      { cause: error },
    );
  } finally {
    clearTimeout(timeout);
  }
}

function tlsPinsForHost(keyset: WorkloadKeyset, hostname: string): string[] {
  const entries = Array.isArray(keyset.tls_public_keys) ? keyset.tls_public_keys : [];
  const normalizedHost = normalizeHostname(hostname);
  const pins = entries
    .filter(
      (pin) => pin.domain === undefined || normalizeHostname(pin.domain) === normalizedHost,
    )
    .map((pin) => pin.spki_sha256.toLowerCase());
  if (pins.length === 0) {
    throw new AciConnectionError(
      'invalid_tls_pin',
      `ACI workload keyset has no valid TLS SPKI for ${hostname}`,
    );
  }
  if (pins.some((pin) => !/^[0-9a-f]{64}$/.test(pin))) {
    throw new AciConnectionError(
      'invalid_tls_pin',
      `ACI workload keyset has an invalid TLS SPKI for ${hostname}`,
    );
  }
  return [...new Set(pins)];
}

function normalizeHostname(value: string): string {
  return value.trim().toLowerCase().replace(/\.$/, '');
}

function normalizeBaseURL(value: string): {
  baseURL: string;
  origin: string;
  hostname: string;
  attestationURL: string;
} {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new AciConnectionError('invalid_base_url', 'baseURL must be an absolute HTTPS URL');
  }
  if (url.protocol !== 'https:') {
    throw new AciConnectionError('invalid_base_url', 'baseURL must use HTTPS');
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new AciConnectionError(
      'invalid_base_url',
      'baseURL must not contain credentials, query parameters, or a fragment',
    );
  }
  url.pathname = url.pathname.replace(/\/+$/, '');
  const baseURL = url.toString().replace(/\/$/, '');
  const rootPath = url.pathname.replace(/\/v\d+$/, '').replace(/\/+$/, '');
  const attestation = new URL(url.origin);
  attestation.pathname = `${rootPath}/v1/aci/attestation`;
  return {
    baseURL,
    origin: url.origin,
    hostname: url.hostname.toLowerCase().replace(/\.$/, ''),
    attestationURL: attestation.toString(),
  };
}
