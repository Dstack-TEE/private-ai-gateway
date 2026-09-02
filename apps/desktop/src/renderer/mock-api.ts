import type {
  AgentPreview,
  AgentStatus,
  DesktopApi,
  GatewayState,
} from "../shared/contracts";

/**
 * Stateful in-browser stand-in for the desktop bridge (`?mock=<scenario>`),
 * derived from the shared contract types so a contract change breaks it at
 * compile time. Screenshots use the canned scenarios; `interactive` starts
 * cold and implements the real transitions (start/stop, save/delete key,
 * connect/disconnect, restore all) so the renderer flow can be exercised by a
 * browser test without a backend.
 */
export type MockScenario =
  | "ready"
  | "no-key"
  | "verifying"
  | "error"
  | "empty-catalog"
  | "needs-attention"
  | "endpoint-busy"
  | "interactive";

const now = Math.floor(Date.now() / 1000);

const BASE: GatewayState = {
  status: "stopped",
  proxyUrl: "https://127.0.0.1:4180",
  checks: [],
  activity: [],
  config: { remoteUrl: "https://tee.redpill.ai", requireProductionOs: false },
  apiKeySaved: true,
};

const IDENTITY: GatewayState["identity"] = {
  teeType: "tdx",
  trustLevel: "hardware_verified",
  keysetDigest: "sha256:6f1c0d9e5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d",
  keysetNotAfter: now + 86_400,
  source: { repoCommit: "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d" },
  serving: "aggregator",
  supportedE2eeVersions: ["2"],
};

const CHECKS: GatewayState["checks"] = [
  { id: "id-1", section: "9.1(1)", title: "hardware quote", status: "pass", detail: "TDX quote verified" },
  { id: "id-4", section: "9.1(4)", title: "source provenance", status: "pass", detail: "compose matches" },
  { id: "policy-os", section: "1.3", title: "production os", status: "skip", detail: "not required" },
];

const CATALOG: NonNullable<GatewayState["catalog"]> = {
  revision: "abc",
  fetchedAt: now,
  removed: [],
  models: [
    { id: "openai/gpt-oss-20b", name: "OpenAI: GPT OSS 20B", contextLength: 131072, chatCompletions: { level: "declared", version: 1 }, messages: { level: "declared", version: 1 }, responses: { level: "undeclared" } },
    { id: "deepseek/deepseek-v4-flash-0731", name: "DeepSeek: DeepSeek V4 Flash 0731", contextLength: 1048576, chatCompletions: { level: "declared", version: 1 }, messages: { level: "declared", version: 1 }, responses: { level: "undeclared" } },
    { id: "phala/qwen3.6-35b-a3b-uncensored-long-model-identifier", name: "Phala: Qwen 3.6 35B", contextLength: 262144, chatCompletions: { level: "declared", version: 1 }, messages: { level: "declared", version: 1 }, responses: { level: "undeclared" } },
  ],
};

const ACTIVITY: GatewayState["activity"] = [
  { method: "POST", path: "/v1/messages", status: 200, streamed: true, receiptId: "rcpt-51be02", verified: true, detail: "receipt verified", at: now - 80, agent: "claude-code", route: "declared", locallyConstrained: true, rewritten: true },
  { method: "POST", path: "/v1/messages", status: 200, streamed: true, receiptId: "rcpt-7f3a9c", verified: true, detail: "receipt verified", at: now - 120, agent: "claude-code", route: "declared", locallyConstrained: true, rewritten: false },
  { method: "POST", path: "/v1/messages", status: 404, streamed: false, verified: null, detail: "`claude-sonnet-4-6` is not in the verified model list", at: now - 200, agent: "claude-code" },
  { method: "GET", path: "/v1/models", status: 401, streamed: false, verified: null, detail: "This endpoint accepts only agents connected through Private AI Gateway", at: now - 260 },
];

const CODEX: AgentStatus = {
  id: "codex",
  name: "Codex",
  configPath: "/Users/dev/.codex/config.toml",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
  surface: "responses",
  supported: false,
  reason: "The verified service publishes no Codex model metadata; Codex's strict catalog needs per-model instructions it ships only for its own models, so this version cannot connect Codex honestly",
};
const CLAUDE: AgentStatus = {
  id: "claude-code",
  name: "Claude Code",
  configPath: "/Users/dev/.claude/settings.json",
  installed: true,
  connected: true,
  recorded: true,
  authorized: true,
  surface: "messages",
  supported: true,
};
const OPENCODE: AgentStatus = {
  id: "opencode",
  name: "OpenCode",
  configPath: "/Users/dev/.config/opencode/opencode.json",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
  surface: "chat_completions",
  supported: false,
  reason: "OpenCode has no configuration-level way to trust the local gateway certificate (only a shell-exported NODE_EXTRA_CA_CERTS would, which the app cannot verify), so this version cannot connect OpenCode",
};
const CLAUDE_OFF: AgentStatus = { ...CLAUDE, connected: false, recorded: false, authorized: false };

function scenario(name: MockScenario): { state: GatewayState; agents: AgentStatus[] } {
  switch (name) {
    case "ready":
      return {
        state: { ...BASE, status: "verified", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, catalog: CATALOG, activity: ACTIVITY },
        agents: [CODEX, CLAUDE, OPENCODE],
      };
    case "no-key":
      return {
        state: { ...BASE, status: "verified", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, catalog: CATALOG, apiKeySaved: false },
        agents: [CODEX, CLAUDE_OFF, OPENCODE],
      };
    case "verifying":
      return {
        state: { ...BASE, status: "verifying", progress: "Reading the verified model list", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS },
        agents: [CODEX, CLAUDE_OFF, OPENCODE],
      };
    case "error":
      return {
        state: { ...BASE, status: "error", remoteUrl: BASE.config.remoteUrl, error: "Cannot read the verified model list: The verified gateway did not answer the model list request" },
        agents: [CODEX, CLAUDE_OFF, OPENCODE],
      };
    case "empty-catalog":
      return {
        state: { ...BASE, status: "error", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, error: "Cannot read the verified model list: the service returned no models" },
        agents: [CODEX, CLAUDE_OFF, OPENCODE],
      };
    case "needs-attention":
      return {
        state: {
          ...BASE,
          status: "verified",
          remoteUrl: BASE.config.remoteUrl,
          identity: IDENTITY,
          checks: CHECKS,
          activity: ACTIVITY,
          catalog: { ...CATALOG, models: CATALOG.models.slice(1), removed: ["openai/gpt-oss-20b"] },
        },
        agents: [
          { ...CODEX, connected: true, recorded: true, attention: "Connected by an earlier version that this build no longer supports; access is disabled. Disconnect to restore your config" },
          { ...CLAUDE, authorized: false, attention: "The config no longer matches what the app wrote (edited outside the app, or unreadable); this agent's access is disabled. Disconnect to clean up, or reconnect" },
          { ...OPENCODE, recorded: true, attention: "Disconnect did not complete; this agent's access is disabled until Disconnect is retried" },
        ],
      };
    case "endpoint-busy":
      return {
        state: { ...BASE, proxyUrl: undefined, endpointError: "Cannot listen on 127.0.0.1:4180: Address already in use (os error 48)" },
        agents: [CODEX, CLAUDE, OPENCODE],
      };
    case "interactive":
      return {
        state: { ...BASE, apiKeySaved: false, remoteUrl: BASE.config.remoteUrl },
        agents: [CODEX, CLAUDE_OFF, OPENCODE],
      };
  }
}

export function mockApi(name: string | null): DesktopApi {
  const known: MockScenario[] = ["ready", "no-key", "verifying", "error", "empty-catalog", "needs-attention", "endpoint-busy", "interactive"];
  const picked = known.find((candidate) => candidate === name) ?? "ready";
  let { state, agents } = scenario(picked);
  const listeners = new Set<(state: GatewayState) => void>();
  // Each start gets its own verification run; stop or a newer start makes a
  // pending timer a no-op instead of completing the wrong run.
  let verifyRun = 0;
  const publish = () => listeners.forEach((listener) => listener(state));
  const claude = () => agents.find((agent) => agent.id === "claude-code") ?? CLAUDE_OFF;
  return {
    copyText: async () => undefined,
    getState: async () => state,
    onStateChange: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    start: async (config) => {
      const run = ++verifyRun;
      state = { ...state, status: "verifying", progress: "Starting the verifier", config, remoteUrl: config.remoteUrl, error: undefined };
      publish();
      window.setTimeout(() => {
        if (run !== verifyRun || state.status !== "verifying") {
          return;
        }
        // An unreachable service (`*.invalid` is reserved as never-resolving)
        // fails verification like the real backend would.
        state = config.remoteUrl.endsWith(".invalid")
          ? { ...state, status: "error", progress: undefined, error: "The verified gateway did not answer the model list request" }
          : { ...state, status: "verified", progress: undefined, identity: IDENTITY, checks: CHECKS, catalog: CATALOG };
        publish();
      }, 350);
      return state;
    },
    stop: async () => {
      verifyRun += 1;
      state = { ...state, status: "stopped", progress: undefined, identity: undefined, checks: [], catalog: undefined };
      publish();
      return state;
    },
    setApiKey: async (key) => {
      if (!key.trim()) {
        throw new Error("Enter an API key");
      }
      state = { ...state, apiKeySaved: true };
      publish();
      return state;
    },
    clearApiKey: async () => {
      state = { ...state, apiKeySaved: false };
      publish();
      return state;
    },
    refreshCatalog: async () => state,
    listAgents: async () => agents,
    disconnectAllAgents: async () => {
      agents = agents.map((agent) => ({ ...agent, connected: false, recorded: false, authorized: false, attention: undefined }));
      return agents;
    },
    previewAgent: async (agentId, connect, options): Promise<AgentPreview> => {
      const agent = agents.find((candidate) => candidate.id === agentId) ?? CLAUDE_OFF;
      if (connect && !agent.supported) {
        throw new Error(agent.reason ?? "Unsupported");
      }
      if (connect && agentId === "claude-code" && !options.model) {
        throw new Error("Choose a model from the verified list for Claude Code");
      }
      return {
        agent,
        connect,
        revision: "mock",
        note: connect
          ? "Claude Code authenticates through apiKeyHelper with a machine-local token and uses the selected model. Credentials set in this settings file are taken over and restored on disconnect."
          : "Only fields written by Private AI Gateway are restored; the agent's local token is revoked.",
        changes: connect
          ? [
              { key: "env.ANTHROPIC_BASE_URL", before: null, after: "https://127.0.0.1:4180", sensitive: false },
              { key: "env.NODE_EXTRA_CA_CERTS", before: null, after: "/Users/dev/Library/Application Support/ai.redpill.private-ai-gateway/local-gateway-ca.pem", sensitive: false },
              { key: "env.ANTHROPIC_MODEL", before: null, after: options.model ?? "openai/gpt-oss-20b", sensitive: false },
              { key: "apiKeyHelper", before: null, after: "Managed local credential", sensitive: true },
              { key: "env.ANTHROPIC_AUTH_TOKEN", before: "Existing secret", after: null, sensitive: true },
            ]
          : [
              { key: "env.ANTHROPIC_BASE_URL", before: "https://127.0.0.1:4180", after: null, sensitive: false },
              { key: "apiKeyHelper", before: "Managed local credential", after: null, sensitive: true },
              { key: "env.ANTHROPIC_AUTH_TOKEN", before: null, after: "Previous secret restored", sensitive: true },
            ],
      };
    },
    applyAgent: async (agentId, connect) => {
      agents = agents.map((agent) =>
        agent.id === agentId
          ? { ...agent, connected: connect, recorded: connect, authorized: connect, attention: undefined }
          : agent,
      );
      return agents.find((agent) => agent.id === agentId) ?? claude();
    },
  };
}
