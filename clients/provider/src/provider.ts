import {
  connectAci,
  type AciConnection,
  type AciFetch,
  type AciReceiptAudit,
  type RecordedAciExchange,
  type VerifiedAciIdentity,
} from "@phala/aci-verifier/runtime";

import type { AciProviderConfig } from "./config.ts";
import { discoverAciModels, type AciModel } from "./models.ts";

export type AciProviderPhase = "idle" | "connecting" | "verified" | "blocked" | "closed";

export interface AciProviderStatus {
  phase: AciProviderPhase;
  error?: string;
  identity?: VerifiedAciIdentity;
  models: readonly AciModel[];
  receipts: readonly RecordedAciExchange[];
}

export class AciProviderError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "AciProviderError";
  }
}

export class AciProvider {
  readonly fetch: AciFetch;
  readonly config: AciProviderConfig;

  private connection?: AciConnection;
  private connecting?: Promise<AciConnection>;
  private phase: AciProviderPhase = "idle";
  private error?: string;
  private catalog: readonly AciModel[] = [];

  constructor(config: AciProviderConfig) {
    this.config = config;
    this.fetch = (input, init) => this.secureFetch(input, init);
  }

  async connect(): Promise<VerifiedAciIdentity> {
    if (this.phase === "closed") throw new AciProviderError("ACI provider is closed");
    if (this.connection) return this.connection.identity;
    if (!this.connecting) {
      this.phase = "connecting";
      const pending = connectAci({
        baseURL: this.config.baseURL,
        policy: this.config.trust.acceptedComposeHashes
          ? { acceptedComposeHashes: this.config.trust.acceptedComposeHashes }
          : {},
        serving: this.config.trust.acceptedSessionIds
          ? { acceptedSessionIds: this.config.trust.acceptedSessionIds }
          : {},
        receiptHistorySize: this.config.receipts.historySize,
      })
        .then((connection) => {
          if (this.phase === "closed") {
            return connection.close().then(() => {
              throw new AciProviderError("ACI provider closed during connection setup");
            });
          }
          this.connection = connection;
          this.phase = "verified";
          this.error = undefined;
          return connection;
        })
        .catch((error: unknown) => {
          if (this.phase !== "closed") {
            this.phase = "blocked";
            this.error = error instanceof Error ? error.message : String(error);
          }
          throw new AciProviderError("ACI connection verification failed", { cause: error });
        })
        .finally(() => {
          if (this.connecting === pending) this.connecting = undefined;
        });
      this.connecting = pending;
    }
    return (await this.connecting).identity;
  }

  async discoverModels(options: { signal?: AbortSignal } = {}): Promise<readonly AciModel[]> {
    await this.connect();
    const models = await discoverAciModels({
      config: this.config,
      fetch: this.fetch,
      ...options,
    });
    this.catalog = models;
    return models;
  }

  models(): readonly AciModel[] {
    return this.catalog;
  }

  receipts(): readonly RecordedAciExchange[] {
    return this.connection?.receipts() ?? [];
  }

  async verifyReceipt(receiptId?: string): Promise<AciReceiptAudit> {
    await this.connect();
    const audit = await this.connection?.verifyReceipt(receiptId);
    if (!audit) throw new AciProviderError("ACI connection is unavailable");
    if (!audit.transcript.verdict.verified) {
      throw new AciProviderError(
        `ACI receipt verification failed: ${audit.transcript.verdict.line}`,
      );
    }
    return audit;
  }

  status(): AciProviderStatus {
    return {
      phase: this.phase,
      ...(this.error ? { error: this.error } : {}),
      ...(this.connection ? { identity: this.connection.identity } : {}),
      models: this.catalog,
      receipts: this.receipts(),
    };
  }

  async close(): Promise<void> {
    if (this.phase === "closed") return;
    this.phase = "closed";
    try {
      await this.connecting;
    } catch {
      // Connection setup already recorded the useful failure.
    }
    const connection = this.connection;
    this.connection = undefined;
    await connection?.close();
  }

  private async secureFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    await this.connect();
    const connection = this.connection;
    if (!connection) throw new AciProviderError("ACI inference blocked: no verified connection");
    const response = await connection.fetch(input, init);
    if (this.config.receipts.verification !== "response") return response;
    const receiptId = response.headers.get("x-receipt-id");
    if (!receiptId) return response;
    return auditResponse(response, () => this.verifyReceipt(receiptId));
  }
}

export function createAciProvider(config: AciProviderConfig): AciProvider {
  return new AciProvider(config);
}

/** @internal Response-stream boundary used by provider adapters and contract tests. */
export function auditResponse(response: Response, verify: () => Promise<unknown>): Response {
  if (!response.body) {
    return new Response(
      new ReadableStream({
        async start(controller) {
          try {
            await verify();
            controller.close();
          } catch (error) {
            controller.error(error);
          }
        },
      }),
      response,
    );
  }
  const body = response.body.pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        controller.enqueue(chunk);
      },
      async flush() {
        await verify();
      },
    }),
  );
  return new Response(body, {
    status: response.status,
    statusText: response.statusText,
    headers: new Headers(response.headers),
  });
}
