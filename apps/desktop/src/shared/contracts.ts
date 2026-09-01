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
  method: string;
  path: string;
  status: number;
  streamed: boolean;
  receiptId?: string;
  verified: boolean | null;
  detail: string;
  at: number;
}

export interface GatewayState {
  status: GatewayStatus;
  remoteUrl?: string;
  proxyUrl?: string;
  controlUrl?: string;
  identity?: GatewayIdentity;
  checks: VerificationCheck[];
  activity: RequestActivity[];
  error?: string;
}

export interface StartGatewayConfig {
  remoteUrl: string;
  requireProductionOs: boolean;
}

export interface DesktopApi {
  copyText(text: string): Promise<void>;
  getState(): Promise<GatewayState>;
  onStateChange(listener: (state: GatewayState) => void): () => void;
  start(config: StartGatewayConfig): Promise<GatewayState>;
  stop(): Promise<GatewayState>;
}
