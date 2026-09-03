export type GatewayStatus =
  | "stopped"
  | "verifying"
  | "verified"
  | "blocked"
  | "error";

export type CheckStatus = "pass" | "fail" | "skip" | "info";

export interface VerificationCheck {
  id: string;
  section: string;
  title: string;
  status: CheckStatus;
  detail: string;
}

export interface SourceProvenance {
  repoUrl?: string;
  repoCommit?: string;
  imageDigest?: string;
}

export interface GatewayIdentity {
  teeType: string;
  trustLevel: string;
  keysetDigest: string;
  keysetNotAfter: number;
  tlsSpki?: string;
  source: SourceProvenance;
  serving: string;
  supportedE2eeVersions: string[];
}

export interface RequestActivity {
  id: string;
  sessionId: string;
  method: string;
  path: string;
  model?: string;
  status: number;
  streamed: boolean;
  receiptId?: string;
  verified: boolean | null;
  detail: string;
  at: number;
  agent?: string;
  /** The verifier applied its ACI policy to the body; the receipt binds those bytes. */
  locallyConstrained?: boolean;
  rewritten?: boolean;
  leftDevice: boolean;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
  costUsd?: number;
}

export interface ModelSummary {
  id: string;
  name: string;
  contextLength?: number;
  maxOutputLength?: number;
  isTee?: boolean;
  inputPricePerMillion?: number;
  outputPricePerMillion?: number;
  cacheReadPricePerMillion?: number;
  cacheWritePricePerMillion?: number;
  inputModalities: string[];
  outputModalities: string[];
  capabilities: string[];
  description?: string;
}

export interface UsageQuery {
  agent?: string;
  model?: string;
  sessionId?: string;
  since?: number;
  until?: number;
  cursor?: string;
  limit?: number;
}

export interface UsageSummary {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  costUsd: number;
  protected: number;
  blockedLocally: number;
  failedProof: number;
}

export interface UsagePoint {
  day: string;
  requests: number;
  inputTokens: number;
  outputTokens: number;
  tokens: number;
  costUsd: number;
}

export interface UsagePage {
  items: RequestActivity[];
  nextCursor?: string;
  summary: UsageSummary;
  series: UsagePoint[];
  agents: string[];
  models: string[];
}

export interface CatalogSummary {
  revision: string;
  fetchedAt: number;
  models: ModelSummary[];
  removed: string[];
}

export type ServiceProvider = "phala" | "redpill" | "custom";

export type ProfileAuth =
  | { kind: "apiKey" }
  | { kind: "oauth"; accountId: string; accountName?: string };

export interface ConfidentialProfile {
  id: string;
  name: string;
  provider: ServiceProvider;
  remoteUrl: string;
  auth: ProfileAuth;
  verifiedAt?: number;
}

export interface ConfidentialProfileInput {
  id: string;
  name: string;
  provider: ServiceProvider;
  remoteUrl: string;
}

export interface StartGatewayConfig {
  remoteUrl: string;
  requireProductionOs: boolean;
}

export interface LocalApiConfig {
  listenAddress: string;
  allowNetworkAccess: boolean;
  port: number;
  clientHost?: string;
}

export interface GatewayState {
  status: GatewayStatus;
  /** Settings is verifying a candidate without enabling forwarding. */
  configurationVerification: boolean;
  /** What the gateway is doing while verifying. */
  progress?: string;
  remoteUrl?: string;
  /** The stable local endpoint agents use; present only while it is bound. */
  proxyUrl?: string;
  /** Why the local endpoint could not be bound; blocks starting and connecting. */
  endpointError?: string;
  identity?: GatewayIdentity;
  checks: VerificationCheck[];
  activity: RequestActivity[];
  sessionId?: string;
  sessionUsage: UsageSummary;
  usageRevision: number;
  error?: string;
  config: StartGatewayConfig;
  profiles: ConfidentialProfile[];
  activeProfileId: string;
  localApi: LocalApiConfig;
  apiKeySaved: boolean;
  catalog?: CatalogSummary;
}

export interface AgentStatus {
  id: string;
  name: string;
  configPath: string;
  installed: boolean;
  connected: boolean;
  /** A connection record exists (whatever the config now says). */
  recorded: boolean;
  /** The proxy would authorize this agent's token right now. */
  authorized: boolean;
  /** Something the user must act on (removed model, incomplete disconnect). */
  attention?: string;
  error?: string;
}

/**
 * One config field a connection changes; `null` means absent. Sensitive
 * fields carry labels instead of values.
 */
export interface ConfigChange {
  key: string;
  before: string | null;
  after: string | null;
  sensitive: boolean;
}

/** User choices a connection is projected with. */
export interface ConnectOptions {
  /** Optional default. The verified catalog remains discoverable by the agent. */
  defaultModel?: string;
}

export interface AgentPreview {
  agent: AgentStatus;
  connect: boolean;
  changes: ConfigChange[];
  note: string;
  /** Fingerprint of the preview inputs; apply refuses when it no longer matches. */
  revision: string;
}

export interface DesktopApi {
  copyText(text: string): Promise<void>;
  getClientKey(): Promise<string>;
  rotateClientKey(): Promise<string>;
  saveLocalApiConfig(config: LocalApiConfig): Promise<GatewayState>;
  getState(): Promise<GatewayState>;
  onStateChange(listener: (state: GatewayState) => void): () => void;
  /** A native menu asked the window to show a section (macOS Settings…). */
  onNavigate(listener: (section: "settings") => void): () => void;
  /** Open the brand's support page in the system browser. */
  openSupport(): Promise<void>;
  start(config: StartGatewayConfig): Promise<GatewayState>;
  verifyConfiguration(profile: ConfidentialProfileInput, requireProductionOs: boolean, key?: string): Promise<GatewayState>;
  activateProfile(profileId: string): Promise<GatewayState>;
  deleteProfile(profileId: string): Promise<GatewayState>;
  stop(): Promise<GatewayState>;
  clearApiKey(): Promise<GatewayState>;
  queryUsage(query: UsageQuery): Promise<UsagePage>;
  exportUsageCsv(query: UsageQuery, path: string): Promise<number>;
  clearUsage(): Promise<number>;
  refreshCatalog(): Promise<GatewayState>;
  listAgents(): Promise<AgentStatus[]>;
  disconnectAllAgents(): Promise<AgentStatus[]>;
  previewAgent(agentId: string, connect: boolean, options: ConnectOptions): Promise<AgentPreview>;
  applyAgent(
    agentId: string,
    connect: boolean,
    revision: string,
    options: ConnectOptions,
  ): Promise<AgentStatus>;
}
