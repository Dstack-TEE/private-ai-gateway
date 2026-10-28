import { createHash, randomBytes } from 'node:crypto';

import { jcsEqual } from '../crypto.js';

import {
  bindReportTranscriptChannel,
  computeVerdict,
  receiptTranscriptFromDigests,
  reportTranscript,
} from '../transcript.js';
import type { AttestationReport, ReceiptEnvelope, SessionRecord, SourceProvenance, WorkloadKeyset } from '../types.js';
import type {
  PinnedTransport,
  PinnedTransportFactory,
} from './transport.js';
import {
  AciConnectionError,
  type AciFetch,
  type AciConnection,
  type AciReceiptAudit,
  type ConnectAciOptions,
  type RecordedAciExchange,
  type VerifiedAciIdentity,
} from './types.js';

import { constrainRequestBody, object, validateProviderHeaderMultiplicity } from './constraints.js';
export { constrainJsonBody } from './constraints.js';

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_RECEIPT_HISTORY_SIZE = 32;

interface InternalExchange extends RecordedAciExchange {
  requestDigest: string;
  responseDigest?: string;
  /** Original request credential, retained only for its private receipt lookup. */
  authorization?: string;
  completion: Promise<void>;
  identity: VerifiedAciIdentity;
  members: ReadonlyMap<string, VerifiedAciIdentity>;
  pinnedSessions: string[];
  requiresVerified: boolean;
}

export async function connectAciWithTransport(
  options: ConnectAciOptions,
  createPinnedTransport: PinnedTransportFactory,
  identityProvider: IdentityProvider = establishIdentity,
): Promise<AciConnection> {
  const connection = new RuntimeAciConnection(options, createPinnedTransport, identityProvider);
  await connection.refresh();
  return connection;
}

type IdentityProvider = (
  attestationURL: string,
  origin: string,
  hostname: string,
  options: ConnectAciOptions,
) => Promise<VerifiedAciIdentity>;

class RuntimeAciConnection implements AciConnection {
  readonly baseURL: string;
  readonly fetch: AciFetch;

  private readonly origin: string;
  private readonly hostname: string;
  private readonly attestationURL: string;
  private readonly options: ConnectAciOptions;
  private readonly createPinnedTransport: PinnedTransportFactory;
  private readonly identityProvider: IdentityProvider;
  private currentIdentity?: VerifiedAciIdentity;
  private transport: PinnedTransport | undefined;
  private refreshing: Promise<boolean> | undefined;
  private members = new Map<string, VerifiedAciIdentity>();
  private readonly exchanges: InternalExchange[] = [];
  private pendingDigest: string | undefined;
  private closed = false;

  constructor(
    options: ConnectAciOptions,
    createPinnedTransport: PinnedTransportFactory,
    identityProvider: IdentityProvider,
  ) {
    validateOptions(options);
    const target = normalizeBaseURL(options.baseURL);
    this.baseURL = target.baseURL;
    this.origin = target.origin;
    this.hostname = target.hostname;
    this.attestationURL = target.attestationURL;
    this.options = options;
    this.createPinnedTransport = createPinnedTransport;
    this.identityProvider = identityProvider;
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
    const receipt = await this.fetchJsonArtifact<ReceiptEnvelope>(
      `receipts/${encodeURIComponent(id)}`,
      exchange.authorization,
    );
    let entry = this.receiptIdentity(receipt, exchange);
    if (!entry) {
      if (typeof receipt.workload_keyset_digest === 'string') {
        this.observeDigest(receipt.workload_keyset_digest);
      }
      await this.ensureFreshIdentity();
      entry = this.receiptIdentity(receipt, exchange);
    }
    if (!entry) {
      throw new AciConnectionError(
        'receipt_verification',
        'receipt names a digest not established for the admitted renewal family',
      );
    }
    const sessionId = citedSessionId(receipt);
    let session: SessionRecord | undefined;
    if (sessionId) {
      try {
        session = await this.fetchJsonArtifact<SessionRecord>(`sessions/${encodeURIComponent(sessionId)}`);
      } catch (error) {
        if (!(error instanceof AciConnectionError) || error.code !== 'receipt_fetch') throw error;
      }
    }
    const serving = entry.report.service_capabilities?.serving;
    const transcript = await receiptTranscriptFromDigests(
      receipt,
      entry.keyset,
      entry.workloadKeysetDigest,
      {
        request: exchange.requestDigest,
        ...(exchange.responseDigest ? { response: exchange.responseDigest } : {}),
      },
      {
        ...(session === undefined ? {} : { session }),
        ...(exchange.pinnedSessions.length ? { pinnedSessions: exchange.pinnedSessions } : {}),
        requiresVerified: exchange.requiresVerified,
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

  async refresh(): Promise<void> {
    await this.refreshIdentity();
  }

  private refreshIdentity(): Promise<boolean> {
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

  private async refreshOnce(): Promise<boolean> {
    const identity = await this.identityProvider(
      this.attestationURL,
      this.origin,
      this.hostname,
      this.options,
    );
    const candidate = this.createPinnedTransport({
      origin: this.origin,
      hostname: this.hostname,
      spkiPins: identity.tlsSpkiPins,
      ...(this.options.proxy === undefined ? {} : { proxy: this.options.proxy }),
      ...(this.options.ca === undefined ? {} : { ca: this.options.ca }),
    });
    let multipartEndpoints: string[];
    try {
      multipartEndpoints = await probePinnedChannel(candidate, this.attestationURL, this.options);
      if (this.closed) throw new AciConnectionError('closed', 'ACI connection closed during refresh');
    } catch (error) {
      await candidate.close();
      if (error instanceof AciConnectionError) throw error;
      throw new AciConnectionError('channel_binding', 'failed to establish the pinned ACI channel', {
        cause: error,
      });
    }

    const observedSpkiSha256 = candidate.observedSpkiSha256();
    if (!observedSpkiSha256) {
      await candidate.close();
      throw new AciConnectionError(
        'channel_binding',
        'pinned ACI channel probe did not observe a TLS peer SPKI',
      );
    }
    const transcript = bindReportTranscriptChannel(identity.transcript, {
      observedSpkiSha256,
      host: identity.hostname,
    });
    const channelCheck = transcript.lines.find((line) => line.id === 'id-6');
    if (channelCheck?.status !== 'pass') {
      await candidate.close();
      throw new AciConnectionError(
        'channel_binding',
        `pinned ACI channel did not satisfy id-6: ${channelCheck?.detail ?? 'check did not run'}`,
      );
    }
    const boundIdentity: VerifiedAciIdentity = {
      ...identity,
      transcript,
      report: {
        ...identity.report,
        service_capabilities: {
          ...identity.report.service_capabilities,
          supported_multipart_constraint_endpoints: multipartEndpoints,
        },
      },
    };

    const previous = this.transport;
    const previousIdentity = this.currentIdentity;
    const renewal = previousIdentity !== undefined
      && sameIdentityFamily(boundIdentity, previousIdentity);
    const keepTransport = previous !== undefined && renewal;
    const now = Date.now();
    const members = renewal ? new Map(this.members) : new Map<string, VerifiedAciIdentity>();
    for (const [digest, member] of members) {
      if (now >= member.expiresAt) members.delete(digest);
    }
    members.set(boundIdentity.workloadKeysetDigest, boundIdentity);
    this.members = members;
    this.transport = keepTransport ? previous : candidate;
    this.currentIdentity = boundIdentity;
    // Any freshly verified member authorizes admission. Receipt audits still
    // require the exact digest to have been attested within the admitted family.
    this.pendingDigest = undefined;
    if (keepTransport) await candidate.close();
    else if (previous) await previous.close();
    return previousIdentity !== undefined && !renewal;
  }

  private async secureFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    if (this.closed || !this.transport) {
      throw new AciConnectionError('closed', 'ACI connection is closed');
    }
    await this.ensureFreshIdentity();
    const transport = this.transport;
    if (!transport) throw new AciConnectionError('closed', 'ACI connection is closed');
    const identity = this.identity;
    const members = this.members;
    const prepared = await prepareRequest(input, init, this.options, identity.report.service_capabilities);
    const response = await transport.fetch(prepared.request);
    const observedDigest = response.headers.get('x-aci-keyset-digest');
    if (observedDigest) this.observeDigest(observedDigest);
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
      this.recordExchange(receiptId, prepared, response, identity, members);
    }
    return response;
  }

  private async ensureFreshIdentity(): Promise<void> {
    const expired = ![...this.members.values()].some((member) => Date.now() < member.expiresAt);
    if (!this.pendingDigest && !expired) return;
    let rotated: boolean;
    try {
      rotated = await this.refreshIdentity();
    } catch (error) {
      throw new AciConnectionError(
        expired ? 'identity_expired' : 'attestation_verification',
        expired
          ? 'ACI workload identity expired and re-verification failed'
          : 'ACI response advertised an unestablished digest and re-verification failed',
        { cause: error },
      );
    }
    if (rotated) {
      throw new AciConnectionError(
        'identity_expired',
        'ACI workload identity rotated; retry after establishing the new identity',
      );
    }
  }

  private observeDigest(digest: string): void {
    const member = this.members.get(digest);
    if (!member || Date.now() >= member.expiresAt) this.pendingDigest = digest;
  }

  private receiptIdentity(
    receipt: ReceiptEnvelope,
    exchange: InternalExchange,
  ): VerifiedAciIdentity | undefined {
    const digest = receipt.workload_keyset_digest;
    if (typeof digest !== 'string') return undefined;
    const member = exchange.members.get(digest) ?? this.members.get(digest);
    return member && sameIdentityFamily(member, exchange.identity) ? member : undefined;
  }

  private recordExchange(
    receiptId: string,
    prepared: PreparedRequest,
    response: Response,
    identity: VerifiedAciIdentity,
    members: ReadonlyMap<string, VerifiedAciIdentity>,
  ): void {
    const entry: InternalExchange = {
      receiptId,
      method: prepared.request.method,
      path: new URL(prepared.request.url).pathname,
      status: response.status,
      recordedAt: Date.now(),
      responseComplete: false,
      requestDigest: prepared.bodyDigest,
      ...(prepared.authorization === undefined ? {} : { authorization: prepared.authorization }),
      completion: Promise.resolve(),
      identity,
      members,
      pinnedSessions: prepared.pinnedSessions,
      requiresVerified: prepared.requiresVerified,
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

  private async fetchJsonArtifact<T extends object>(path: string, authorization?: string): Promise<T> {
    const root = new URL(this.attestationURL);
    root.pathname = root.pathname.replace(/attestation$/, path);
    const response = await this.secureFetch(root, {
      headers: {
        Accept: 'application/json',
        ...(authorization ? { Authorization: authorization } : {}),
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
  requiresVerified: boolean;
  authorization?: string;
}

/** @internal Exported for transport-policy conformance tests. */
export async function prepareRequest(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  options: ConnectAciOptions,
  capabilities?: AttestationReport['service_capabilities'],
): Promise<PreparedRequest> {
  validateProviderHeaderMultiplicity(init?.headers);
  const original = new Request(input, init);
  const authorization = original.headers.get('authorization')?.trim() || undefined;
  if (original.method !== 'POST') {
    return {
      request: original,
      bodyDigest: await digestRequest(original),
      pinnedSessions: [],
      requiresVerified: false,
      ...(authorization ? { authorization } : {}),
    };
  }
  const bytes = new Uint8Array(await original.clone().arrayBuffer());
  const headers = new Headers(original.headers);
  const constrained = await constrainRequestBody(
    bytes, headers, new URL(original.url).pathname, options, capabilities,
  );
  headers.delete('content-length');
  const request = new Request(original, { method: 'POST', body: Buffer.from(constrained.body), headers });
  return {
    request,
    bodyDigest: digestBytes(constrained.body),
    pinnedSessions: constrained.pinnedSessions,
    requiresVerified: constrained.requiresVerified,
    ...(authorization ? { authorization } : {}),
  };
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
): Promise<string[]> {
  const url = new URL(attestationURL);
  url.searchParams.set('nonce', randomBytes(32).toString('hex'));
  const response = await timedFetch(
    transport.fetch,
    url,
    {
      headers: {
        Accept: 'application/json',
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
  try {
    const report: unknown = await response.json();
    if (!object(report)) return [];
    const capabilities = report.service_capabilities;
    if (!object(capabilities)) return [];
    const endpoints = capabilities.supported_multipart_constraint_endpoints;
    return Array.isArray(endpoints) && endpoints.every((path): path is string => typeof path === 'string')
      ? endpoints : [];
  } catch (error) {
    throw new AciConnectionError('channel_binding', 'pinned ACI attestation is not valid JSON', { cause: error });
  }
}

async function timedFetch(
  fetchImpl: AciFetch,
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

// Normalize the protocol-defined provenance struct, as Rust deserialization does.
function sourceProvenance(identity: VerifiedAciIdentity): SourceProvenance {
  const source = identity.report.attestation.source_provenance;
  return {
    repo_url: source?.repo_url ?? null,
    repo_commit: source?.repo_commit ?? null,
    image_digest: source?.image_digest ?? null,
    image_provenance: source?.image_provenance ?? null,
  };
}

// Origin and verifier policy are fixed by the owning connection.
function sameIdentityFamily(identity: VerifiedAciIdentity, previous: VerifiedAciIdentity): boolean {
  return sameKeysetFamily(identity.keyset, previous.keyset)
    && identity.composeHash === previous.composeHash
    && identity.report.attestation.tee_type === previous.report.attestation.tee_type
    && jcsEqual(sourceProvenance(identity), sourceProvenance(previous));
}

function sameKeysetFamily(keyset: WorkloadKeyset, previous: WorkloadKeyset): boolean {
  const { not_after: _currentExpiry, ...current } = keyset;
  const { not_after: _previousExpiry, ...old } = previous;
  return jcsEqual(current, old);
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
