import { spawn, type ChildProcessByStdio } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import type { Readable } from "node:stream";

import type {
  CheckStatus,
  GatewayIdentity,
  GatewayState,
  ReceiptSummary,
  RequestActivity,
  StartGatewayConfig,
  VerificationCheck,
} from "../shared/contracts";

const EVENT_SCHEMA_VERSION = 1;
const MAX_ACTIVITY = 30;
const MAX_DIAGNOSTIC_LENGTH = 4_096;

interface AciSidecarOptions {
  executablePath: string;
  env?: NodeJS.ProcessEnv;
  startupTimeoutMs?: number;
}

type StateListener = (state: GatewayState) => void;

export class AciSidecar {
  private readonly executablePath: string;
  private readonly env: NodeJS.ProcessEnv;
  private readonly startupTimeoutMs: number;
  private readonly listeners = new Set<StateListener>();
  private child?: ChildProcessByStdio<null, Readable, Readable>;
  private stopping = false;
  private lastDiagnostic = "";
  private state: GatewayState = {
    status: "stopped",
    checks: [],
    activity: [],
  };

  public constructor(options: AciSidecarOptions) {
    this.executablePath = options.executablePath;
    this.env = options.env ?? process.env;
    this.startupTimeoutMs = options.startupTimeoutMs ?? 60_000;
  }

  public getState(): GatewayState {
    return cloneState(this.state);
  }

  public subscribe(listener: StateListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  public async start(config: StartGatewayConfig): Promise<GatewayState> {
    if (this.child) {
      throw new Error("Gateway is already running");
    }
    const remoteUrl = validateRemoteUrl(config.remoteUrl);
    const args = [
      ...(config.requireProductionOs ? ["--require-production-os"] : []),
      "serve",
      remoteUrl,
      "--listen",
      "127.0.0.1:0",
      "--control",
      "127.0.0.1:0",
      "--json-events",
    ];

    this.stopping = false;
    this.lastDiagnostic = "";
    this.updateState({
      status: "verifying",
      remoteUrl,
      checks: [],
      activity: [],
      error: undefined,
      proxyUrl: undefined,
      controlUrl: undefined,
      identity: undefined,
    });

    const child = spawn(this.executablePath, args, {
      env: this.env,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    this.child = child;

    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => this.handleLine(line));
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      this.lastDiagnostic = `${this.lastDiagnostic}${chunk}`.slice(-MAX_DIAGNOSTIC_LENGTH);
    });

    let startupTimer: NodeJS.Timeout | undefined;
    const ready = new Promise<GatewayState>((resolve, reject) => {
      const unsubscribe = this.subscribe((state) => {
        if (state.status === "verified") {
          unsubscribe();
          resolve(state);
        } else if (state.status === "error" || state.status === "blocked") {
          unsubscribe();
          reject(new Error(state.error ?? "Gateway failed to start"));
        }
      });
      startupTimer = setTimeout(() => {
        unsubscribe();
        reject(new Error("Timed out waiting for ACI verification"));
      }, this.startupTimeoutMs);
      startupTimer.unref();
    });

    child.once("error", (error) => {
      this.fail(`Cannot start bundled ACI executable: ${error.message}`);
    });
    child.once("close", (code, signal) => {
      lines.close();
      if (this.child === child) {
        this.child = undefined;
      }
      if (this.stopping) {
        this.updateState({ status: "stopped", error: undefined });
        return;
      }
      if (this.state.status !== "error" && this.state.status !== "blocked") {
        const suffix = signal ? `signal ${signal}` : `status ${code ?? "unknown"}`;
        const diagnostic = this.lastDiagnostic.trim();
        this.fail(
          diagnostic
            ? `ACI exited with ${suffix}: ${diagnostic}`
            : `ACI exited with ${suffix}`,
        );
      }
    });

    try {
      return await ready;
    } catch (error) {
      await this.stop();
      throw error;
    } finally {
      if (startupTimer) {
        clearTimeout(startupTimer);
      }
    }
  }

  public async stop(): Promise<GatewayState> {
    const child = this.child;
    if (!child) {
      this.updateState({
        status: "stopped",
        proxyUrl: undefined,
        controlUrl: undefined,
        identity: undefined,
        checks: [],
        error: undefined,
      });
      return this.getState();
    }

    this.stopping = true;
    const closed = once(child, "close");
    child.kill();
    const forced = new Promise<void>((resolve) => {
      const timeout = setTimeout(() => {
        if (child.exitCode === null && child.signalCode === null) {
          child.kill("SIGKILL");
        }
        resolve();
      }, 3_000);
      timeout.unref();
    });
    await Promise.race([closed, forced]);
    this.child = undefined;
    this.stopping = false;
    this.updateState({
      status: "stopped",
      proxyUrl: undefined,
      controlUrl: undefined,
      identity: undefined,
      checks: [],
      error: undefined,
    });
    return this.getState();
  }

  public async listReceipts(): Promise<ReceiptSummary[]> {
    const controlUrl = this.state.controlUrl;
    if (!controlUrl) {
      return [];
    }
    const response = await fetch(`${controlUrl}/receipts`, {
      signal: AbortSignal.timeout(3_000),
    });
    if (!response.ok) {
      throw new Error(`Receipt endpoint returned HTTP ${response.status}`);
    }
    return parseReceipts(await response.json());
  }

  private handleLine(line: string): void {
    if (line.length > 1_048_576) {
      this.fail("ACI emitted an oversized event");
      return;
    }
    let event: unknown;
    try {
      event = JSON.parse(line) as unknown;
    } catch {
      this.fail("ACI emitted invalid JSON event data");
      return;
    }
    if (!isRecord(event) || event.schema_version !== EVENT_SCHEMA_VERSION) {
      this.fail("ACI emitted an unsupported event schema");
      return;
    }
    if (event.type === "ready") {
      const ready = parseReadyEvent(event);
      if (!ready) {
        this.fail("ACI emitted an invalid ready event");
        return;
      }
      this.updateState({ ...ready, status: "verified", error: undefined });
      return;
    }
    if (event.type === "identity_updated") {
      const identity = parseIdentityEvent(event);
      if (!identity) {
        this.fail("ACI emitted an invalid identity update");
        return;
      }
      this.updateState({ ...identity, status: "verified", error: undefined });
      return;
    }
    if (event.type === "request_complete") {
      const activity = parseRequestEvent(event);
      if (activity) {
        this.updateState({
          activity: [activity, ...this.state.activity].slice(0, MAX_ACTIVITY),
        });
      }
      return;
    }
    if (event.type === "blocked") {
      this.updateState({
        status: "blocked",
        error: optionalString(event.reason) ?? "ACI blocked forwarding",
      });
      return;
    }
    if (event.type === "fatal") {
      this.fail(optionalString(event.message) ?? "ACI failed");
    }
  }

  private fail(message: string): void {
    this.updateState({ status: "error", error: message });
  }

  private updateState(patch: Partial<GatewayState>): void {
    this.state = { ...this.state, ...patch };
    const snapshot = this.getState();
    for (const listener of this.listeners) {
      listener(snapshot);
    }
  }
}

function validateRemoteUrl(value: string): string {
  const trimmed = value.trim();
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new Error("Gateway URL must be a valid HTTP or HTTPS URL");
  }
  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error("Gateway URL must use HTTP or HTTPS");
  }
  if (url.username || url.password) {
    throw new Error("Gateway URL must not contain credentials");
  }
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}

function parseReadyEvent(event: Record<string, unknown>): Partial<GatewayState> | undefined {
  const remoteUrl = optionalString(event.remote_url);
  const proxyUrl = optionalString(event.proxy_url);
  const controlUrl = optionalString(event.control_url);
  const identity = parseIdentityEvent(event);
  if (!remoteUrl || !proxyUrl || !controlUrl || !identity) {
    return undefined;
  }
  return { remoteUrl, proxyUrl, controlUrl, ...identity };
}

function parseIdentityEvent(event: Record<string, unknown>): Partial<GatewayState> | undefined {
  const teeType = optionalString(event.tee_type);
  const trustLevel = optionalString(event.trust_level);
  const keysetDigest = optionalString(event.keyset_digest);
  const keysetNotAfter = optionalNumber(event.keyset_not_after);
  if (!teeType || !trustLevel || !keysetDigest || keysetNotAfter === undefined) {
    return undefined;
  }
  const source = isRecord(event.source_provenance) ? event.source_provenance : {};
  const capabilities = isRecord(event.service_capabilities)
    ? event.service_capabilities
    : {};
  const identity: GatewayIdentity = {
    teeType,
    trustLevel,
    keysetDigest,
    keysetNotAfter,
    tlsSpki: optionalString(event.tls_spki),
    source: {
      repoUrl: optionalString(source.repo_url),
      repoCommit: optionalString(source.repo_commit),
      imageDigest: optionalString(source.image_digest),
    },
    serving: optionalString(capabilities.serving) ?? "aggregator",
    supportedE2eeVersions: stringArray(capabilities.supported_e2ee_versions),
  };
  return { identity, checks: parseChecks(event.verification) };
}

function parseRequestEvent(event: Record<string, unknown>): RequestActivity | undefined {
  const method = optionalString(event.method);
  const path = optionalString(event.path);
  const status = optionalNumber(event.status);
  if (!method || !path || status === undefined) {
    return undefined;
  }
  return {
    method,
    path,
    status,
    streamed: event.streamed === true,
    verified: typeof event.verified === "boolean" ? event.verified : null,
    detail: optionalString(event.detail) ?? "",
    at: Date.now(),
  };
}

function parseChecks(value: unknown): VerificationCheck[] {
  if (!isRecord(value) || !Array.isArray(value.checks)) {
    return [];
  }
  return value.checks.flatMap((item) => {
    if (!isRecord(item)) {
      return [];
    }
    const id = optionalString(item.id);
    const section = optionalString(item.section);
    const title = optionalString(item.title);
    const status = optionalString(item.status);
    if (!id || !section || !title || !isCheckStatus(status)) {
      return [];
    }
    return [{ id, section, title, status, detail: optionalString(item.detail) ?? "" }];
  });
}

function parseReceipts(value: unknown): ReceiptSummary[] {
  if (!Array.isArray(value)) {
    throw new Error("Receipt endpoint returned invalid data");
  }
  return value.flatMap((item) => {
    if (!isRecord(item)) {
      return [];
    }
    const receiptId = optionalString(item.receipt_id);
    const path = optionalString(item.path);
    const status = optionalNumber(item.status);
    const at = optionalNumber(item.at);
    if (!receiptId || !path || status === undefined || at === undefined) {
      return [];
    }
    return [{
      receiptId,
      path,
      status,
      streamed: item.streamed === true,
      truncated: item.truncated === true,
      at,
      verified: typeof item.verified === "boolean" ? item.verified : null,
    }];
  });
}

function cloneState(state: GatewayState): GatewayState {
  return {
    ...state,
    identity: state.identity
      ? { ...state.identity, source: { ...state.identity.source }, supportedE2eeVersions: [...state.identity.supportedE2eeVersions] }
      : undefined,
    checks: state.checks.map((check) => ({ ...check })),
    activity: state.activity.map((activity) => ({ ...activity })),
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function optionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function isCheckStatus(value: string | undefined): value is CheckStatus {
  return value === "pass" || value === "fail" || value === "skip" || value === "info";
}
