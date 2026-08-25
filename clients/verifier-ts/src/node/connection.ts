import { randomBytes } from 'node:crypto';

import { reportTranscript } from '../transcript.js';
import type { AttestationReport, TlsKeyPin, WorkloadKeyset } from '../types.js';
import { createPinnedTransport, type PinnedTransport } from './transport.js';
import {
  AciConnectionError,
  type AciConnection,
  type ConnectAciOptions,
  type VerifiedAciIdentity,
} from './types.js';

const DEFAULT_TIMEOUT_MS = 10_000;

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
  private closed = false;

  constructor(options: ConnectAciOptions) {
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
      spkiSha256: identity.tlsSpkiSha256,
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
    if (previous) await previous.close();
  }

  private secureFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    if (this.closed || !this.transport) {
      return Promise.reject(new AciConnectionError('closed', 'ACI connection is closed'));
    }
    if (Date.now() >= this.identity.expiresAt) {
      return Promise.reject(
        new AciConnectionError(
          'identity_expired',
          'ACI workload identity expired; call refresh() before sending another request',
        ),
      );
    }
    return this.transport.fetch(input, init);
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
    });
  } catch (error) {
    throw new AciConnectionError(
      'attestation_verification',
      `ACI attestation is malformed: ${error instanceof Error ? error.message : String(error)}`,
      { cause: error },
    );
  }

  const required = ['id-1', 'id-2', 'id-3'];
  if (options.policy?.requireComposeMeasurement !== false) required.push('id-4');
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
  appraiseExpectedSource(report, options);

  const keyset = transcript.verification.keyset;
  const digest = transcript.verification.workloadKeysetDigest;
  if (!keyset || !digest) {
    throw new AciConnectionError(
      'attestation_verification',
      'ACI attestation did not establish a workload keyset',
    );
  }
  const pin = tlsPinForHost(keyset, hostname);
  const verifiedAt = Date.now();
  return {
    origin,
    hostname,
    report,
    keyset,
    workloadKeysetDigest: digest,
    tlsSpkiSha256: pin,
    verifiedAt,
    expiresAt: keyset.not_after * 1000,
    transcript,
  };
}

function appraiseExpectedSource(report: AttestationReport, options: ConnectAciOptions): void {
  const expected = options.policy?.expectedSource;
  if (!expected) return;
  if (!expected.repoUrl && !expected.repoCommit && !expected.imageDigest) {
    throw new AciConnectionError(
      'attestation_verification',
      'policy.expectedSource must contain at least one exact source claim',
    );
  }
  const actual = report.attestation.source_provenance;
  const mismatches: string[] = [];
  if (expected.repoUrl !== undefined && actual?.repo_url !== expected.repoUrl) {
    mismatches.push(`repo_url=${String(actual?.repo_url)}`);
  }
  if (expected.repoCommit !== undefined && actual?.repo_commit !== expected.repoCommit) {
    mismatches.push(`repo_commit=${String(actual?.repo_commit)}`);
  }
  if (expected.imageDigest !== undefined && actual?.image_digest !== expected.imageDigest) {
    mismatches.push(`image_digest=${String(actual?.image_digest)}`);
  }
  if (mismatches.length > 0) {
    throw new AciConnectionError(
      'attestation_verification',
      `ACI source policy rejected the workload: ${mismatches.join(', ')}`,
    );
  }
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

function tlsPinForHost(keyset: WorkloadKeyset, hostname: string): string {
  const entries = Array.isArray(keyset.tls_public_keys) ? keyset.tls_public_keys : [];
  const normalizedHost = hostname.toLowerCase().replace(/\.$/, '');
  const scoped = entries.find(
    (pin) => pin.domain?.toLowerCase().replace(/\.$/, '') === normalizedHost,
  );
  const selected = scoped ?? entries.find((pin: TlsKeyPin) => pin.domain === undefined);
  const value = selected?.spki_sha256.toLowerCase();
  if (!value || !/^[0-9a-f]{64}$/.test(value)) {
    throw new AciConnectionError(
      'invalid_tls_pin',
      `ACI workload keyset has no valid TLS SPKI for ${hostname}`,
    );
  }
  return value;
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
