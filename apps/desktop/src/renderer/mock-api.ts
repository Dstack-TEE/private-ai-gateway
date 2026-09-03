import type {
  AgentPreview,
  AgentStatus,
  DesktopApi,
  GatewayState,
  RequestActivity,
  UsageQuery,
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
  | "blocked"
  | "needs-attention"
  | "endpoint-busy"
  | "interactive";

const now = Math.floor(Date.now() / 1000);

const BASE: GatewayState = {
  status: "stopped",
  proxyUrl: "http://127.0.0.1:4180",
  checks: [],
  activity: [],
  sessionUsage: {
    requests: 0,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
    protected: 0,
    blockedLocally: 0,
    failedProof: 0,
  },
  usageRevision: 0,
  config: { remoteUrl: "https://tee.redpill.ai", requireProductionOs: false },
  localApi: { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 },
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
  { id: "id-6", section: "9.1(6)", title: "channel binding", status: "pass", detail: "TLS key matches the attested keyset" },
  { id: "policy-os", section: "1.3", title: "production os", status: "skip", detail: "not required" },
];

const CATALOG: NonNullable<GatewayState["catalog"]> = {
  revision: "abc",
  fetchedAt: now,
  removed: [],
  models: [
    { id: "openai/gpt-oss-20b", name: "OpenAI: GPT OSS 20B", contextLength: 131072, maxOutputLength: 32768, isTee: true, inputPricePerMillion: 0.08, outputPricePerMillion: 0.35, inputModalities: ["text"], outputModalities: ["text"], capabilities: ["tools", "reasoning"] },
    { id: "deepseek/deepseek-v4-flash-0731", name: "DeepSeek: DeepSeek V4 Flash 0731", contextLength: 1048576, maxOutputLength: 65536, isTee: true, inputPricePerMillion: 0.2, outputPricePerMillion: 0.8, cacheReadPricePerMillion: 0.02, inputModalities: ["text"], outputModalities: ["text"], capabilities: ["tools", "reasoning"] },
    { id: "phala/qwen3.6-35b-a3b-uncensored-long-model-identifier", name: "Phala: Qwen 3.6 35B", contextLength: 262144, maxOutputLength: 32768, isTee: true, inputPricePerMillion: 0.12, outputPricePerMillion: 0.48, inputModalities: ["text"], outputModalities: ["text"], capabilities: ["tools"] },
    { id: "moonshot/kimi-k2.5", name: "Moonshot: Kimi K2.5", contextLength: 262144, maxOutputLength: 32768, isTee: true, inputPricePerMillion: 0.45, outputPricePerMillion: 2.2, cacheReadPricePerMillion: 0.045, inputModalities: ["text", "image"], outputModalities: ["text"], capabilities: ["tools", "vision", "reasoning"] },
    { id: "zai/glm-5.2", name: "Z.ai: GLM 5.2", contextLength: 204800, maxOutputLength: 32768, isTee: true, inputPricePerMillion: 0.3, outputPricePerMillion: 1.2, inputModalities: ["text"], outputModalities: ["text"], capabilities: ["tools", "reasoning"] },
    { id: "meta/llama-4-scout", name: "Meta: Llama 4 Scout", contextLength: 524288, maxOutputLength: 16384, inputPricePerMillion: 0.18, outputPricePerMillion: 0.65, inputModalities: ["text", "image"], outputModalities: ["text"], capabilities: ["tools", "vision"] },
  ],
};

const usage = (item: Partial<RequestActivity> & Pick<RequestActivity, "id" | "method" | "path" | "status" | "at">): RequestActivity => ({
  sessionId: "session-demo",
  streamed: false,
  verified: null,
  detail: "",
  leftDevice: false,
  ...item,
});

const ACTIVITY: GatewayState["activity"] = [
  usage({ id: "51be02", method: "POST", path: "/v1/messages", model: "openai/gpt-oss-20b", status: 200, streamed: true, receiptId: "rcpt-51be02", verified: true, detail: "receipt verified", at: now - 80, agent: "claude-code", locallyConstrained: true, rewritten: true, leftDevice: true, inputTokens: 1260, outputTokens: 284, cacheReadTokens: 800, costUsd: 0.0009 }),
  usage({ id: "7f3a9c", method: "POST", path: "/v1/responses", model: "deepseek/deepseek-v4-flash-0731", status: 200, streamed: true, receiptId: "rcpt-7f3a9c", verified: true, detail: "receipt verified", at: now - 120, agent: "codex", locallyConstrained: true, rewritten: false, leftDevice: true, inputTokens: 880, outputTokens: 412, costUsd: 0.0005 }),
  usage({ id: "local01", method: "POST", path: "/v1/messages", model: "claude-sonnet-4-6", status: 404, detail: "`claude-sonnet-4-6` is not in the verified model list", at: now - 200, agent: "claude-code" }),
  usage({ id: "a50005", method: "POST", path: "/v1/responses", model: "openai/gpt-oss-20b", status: 200, streamed: true, receiptId: "rcpt-a50005", verified: true, detail: "receipt verified", at: now - 320, agent: "pi", leftDevice: true, inputTokens: 450, outputTokens: 90, costUsd: 0.00008 }),
  usage({ id: "c42d18", method: "POST", path: "/v1/chat/completions", model: "zai/glm-5.2", status: 200, receiptId: "rcpt-c42d18", verified: true, detail: "receipt verified", at: now - 410, agent: "hermes", leftDevice: true, inputTokens: 720, outputTokens: 144, costUsd: 0.00039 }),
];

const HISTORY_AGENTS = ["claude-code", "codex", "opencode", "pi", "hermes"] as const;
const HISTORY_MODELS = CATALOG.models.map((model) => model.id);
const USAGE_HISTORY: RequestActivity[] = [
  ...ACTIVITY,
  ...Array.from({ length: 43 }, (_, index) => {
    const agent = HISTORY_AGENTS[index % HISTORY_AGENTS.length];
    const model = HISTORY_MODELS[index % HISTORY_MODELS.length];
    const leftDevice = index % 13 !== 0;
    const deliveryUnconfirmed = index === 19;
    const proofFailed = leftDevice && index % 17 === 0;
    const inputTokens = leftDevice ? 420 + index * 37 : undefined;
    const outputTokens = leftDevice ? 80 + index * 11 : undefined;
    return usage({
      id: `history-${String(index + 1).padStart(2, "0")}`,
      sessionId: `session-${Math.floor(index / 8) + 1}`,
      method: "POST",
      path: index % 3 === 0 ? "/v1/messages" : index % 3 === 1 ? "/v1/responses" : "/v1/chat/completions",
      model,
      status: leftDevice ? (deliveryUnconfirmed ? 504 : index % 19 === 0 ? 429 : 200) : 404,
      streamed: leftDevice,
      receiptId: leftDevice && !deliveryUnconfirmed ? `rcpt-history-${index + 1}` : undefined,
      verified: leftDevice && !deliveryUnconfirmed ? !proofFailed : null,
      detail: leftDevice
        ? deliveryUnconfirmed
          ? "The verified gateway did not respond in time"
          : proofFailed
            ? "receipt signature did not verify"
            : index % 19 === 0
              ? "upstream rate limit"
              : "receipt verified"
        : "model is not in the verified catalog",
      at: now - 3_600 - index * 17_300,
      agent,
      leftDevice,
      inputTokens,
      outputTokens,
      cacheReadTokens: leftDevice && index % 3 === 0 ? 256 + index * 5 : undefined,
      costUsd: leftDevice ? ((inputTokens ?? 0) * 0.0000002) + ((outputTokens ?? 0) * 0.0000008) : undefined,
    });
  }),
].sort((left, right) => right.at - left.at);

const usageSummary = (items: RequestActivity[]): GatewayState["sessionUsage"] => ({
  requests: items.length,
  inputTokens: items.reduce((sum, item) => sum + (item.inputTokens ?? 0), 0),
  outputTokens: items.reduce((sum, item) => sum + (item.outputTokens ?? 0), 0),
  cacheReadTokens: items.reduce((sum, item) => sum + (item.cacheReadTokens ?? 0), 0),
  cacheWriteTokens: items.reduce((sum, item) => sum + (item.cacheWriteTokens ?? 0), 0),
  costUsd: items.reduce((sum, item) => sum + (item.costUsd ?? 0), 0),
  protected: items.filter((item) => item.verified === true).length,
  blockedLocally: items.filter((item) => !item.leftDevice).length,
  failedProof: items.filter((item) => item.verified === false).length,
});

const CODEX: AgentStatus = {
  id: "codex",
  name: "Codex",
  configPath: "/Users/dev/.codex/config.toml",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
};
const CLAUDE: AgentStatus = {
  id: "claude-code",
  name: "Claude Code",
  configPath: "/Users/dev/.claude/settings.json",
  installed: true,
  connected: true,
  recorded: true,
  authorized: true,
};
const OPENCODE: AgentStatus = {
  id: "opencode",
  name: "OpenCode",
  configPath: "/Users/dev/.config/opencode/opencode.json",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
};
const PI: AgentStatus = {
  id: "pi",
  name: "Pi",
  configPath: "/Users/dev/.pi/agent/models.json",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
};
const HERMES: AgentStatus = {
  id: "hermes",
  name: "Hermes",
  configPath: "/Users/dev/.hermes/config.yaml",
  installed: true,
  connected: false,
  recorded: false,
  authorized: false,
};
const CLAUDE_OFF: AgentStatus = { ...CLAUDE, connected: false, recorded: false, authorized: false };
const DEFAULT_AGENTS = [CODEX, CLAUDE, OPENCODE, PI, HERMES];
const STOPPED_AGENTS = [CODEX, CLAUDE_OFF, OPENCODE, PI, HERMES];

function scenario(name: MockScenario): { state: GatewayState; agents: AgentStatus[] } {
  switch (name) {
    case "ready":
      return {
        state: { ...BASE, status: "verified", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, catalog: CATALOG, activity: ACTIVITY, sessionId: "session-demo", sessionUsage: usageSummary(ACTIVITY) },
        agents: DEFAULT_AGENTS,
      };
    case "no-key":
      return {
        state: { ...BASE, status: "verified", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, catalog: CATALOG, apiKeySaved: false },
        agents: STOPPED_AGENTS,
      };
    case "verifying":
      return {
        state: { ...BASE, status: "verifying", progress: "Reading the verified model list", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS },
        agents: STOPPED_AGENTS,
      };
    case "error":
      return {
        state: { ...BASE, status: "error", remoteUrl: BASE.config.remoteUrl, error: "Cannot read the verified model list: The verified gateway did not answer the model list request" },
        agents: STOPPED_AGENTS,
      };
    case "empty-catalog":
      return {
        state: { ...BASE, status: "error", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS, error: "Cannot read the verified model list: the service returned no models" },
        agents: STOPPED_AGENTS,
      };
    case "blocked":
      return {
        state: {
          ...BASE,
          status: "blocked",
          remoteUrl: BASE.config.remoteUrl,
          identity: IDENTITY,
          checks: CHECKS,
          activity: ACTIVITY,
          sessionId: "session-demo",
          sessionUsage: usageSummary(ACTIVITY),
          error: "The service identity changed after verification; forwarding is blocked",
        },
        agents: DEFAULT_AGENTS,
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
          sessionId: "session-demo",
          sessionUsage: usageSummary(ACTIVITY),
          catalog: { ...CATALOG, models: CATALOG.models.slice(1), removed: ["openai/gpt-oss-20b"] },
        },
        agents: [
          { ...CODEX, connected: true, recorded: true, authorized: true, attention: "The selected model is no longer served; choose another model and reconnect" },
          { ...CLAUDE, authorized: false, attention: "The config no longer matches what the app wrote (edited outside the app, or unreadable); this agent's access is disabled. Disconnect to clean up, or reconnect" },
          { ...OPENCODE, recorded: true, attention: "Disconnect did not complete; this agent's access is disabled until Disconnect is retried" },
          PI,
          HERMES,
        ],
      };
    case "endpoint-busy":
      return {
        state: { ...BASE, proxyUrl: undefined, endpointError: "Cannot listen on 127.0.0.1:4180: Address already in use (os error 48)" },
        agents: DEFAULT_AGENTS,
      };
    case "interactive":
      return {
        state: { ...BASE, apiKeySaved: false, remoteUrl: BASE.config.remoteUrl },
        agents: STOPPED_AGENTS,
      };
  }
}

export function mockApi(name: string | null): DesktopApi {
  const known: MockScenario[] = ["ready", "no-key", "verifying", "error", "empty-catalog", "blocked", "needs-attention", "endpoint-busy", "interactive"];
  const picked = known.find((candidate) => candidate === name) ?? "ready";
  let { state, agents } = scenario(picked);
  const listeners = new Set<(state: GatewayState) => void>();
  // Each start gets its own verification run; stop or a newer start makes a
  // pending timer a no-op instead of completing the wrong run.
  let verifyRun = 0;
  let history = [...USAGE_HISTORY];
  let clientKey = "pag_demo_2f8a19c4d7e6b305";
  const publish = () => listeners.forEach((listener) => listener(state));
  const claude = () => agents.find((agent) => agent.id === "claude-code") ?? CLAUDE_OFF;
  return {
    copyText: async () => undefined,
    getClientKey: async () => clientKey,
    rotateClientKey: async () => {
      clientKey = `pag_demo_${Math.random().toString(16).slice(2, 18).padEnd(16, "0")}`;
      return clientKey;
    },
    saveLocalApiConfig: async (config) => {
      if (config.port < 1024 || config.port > 65535) throw new Error("Port must be between 1024 and 65535");
      if (!config.allowNetworkAccess && !["127.0.0.1", "::1"].includes(config.listenAddress)) {
        throw new Error("Turn on Allow network access before listening outside this Mac");
      }
      const host = config.clientHost?.trim() || config.listenAddress;
      const wrapped = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
      state = { ...state, localApi: config, proxyUrl: `http://${wrapped}:${config.port}`, endpointError: undefined };
      publish();
      return state;
    },
    getState: async () => state,
    onStateChange: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    onNavigate: () => () => undefined,
    openSupport: async () => undefined,
    start: async (config) => {
      const run = ++verifyRun;
      state = { ...state, status: "verifying", progress: "Starting the verifier", config, remoteUrl: config.remoteUrl, error: undefined, activity: [], sessionId: `session-mock-${run}`, sessionUsage: usageSummary([]) };
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
    queryUsage: async (query: UsageQuery) => {
      const filtered = history.filter((item) =>
        (!query.agent || item.agent === query.agent)
        && (!query.model || item.model === query.model)
        && (!query.sessionId || item.sessionId === query.sessionId)
        && (!query.since || item.at >= query.since)
        && (!query.until || item.at < query.until));
      const offset = query.cursor ? Number(query.cursor.split(":")[0]) : 0;
      const limit = query.limit ?? 20;
      const items = filtered.slice(offset, offset + limit);
      const tokens = (item: RequestActivity) => (item.inputTokens ?? 0) + (item.outputTokens ?? 0);
      const daily = new Map<string, { requests: number; inputTokens: number; outputTokens: number; tokens: number; costUsd: number }>();
      for (const item of filtered) {
        const day = new Date(item.at * 1_000).toISOString().slice(0, 10);
        const point = daily.get(day) ?? { requests: 0, inputTokens: 0, outputTokens: 0, tokens: 0, costUsd: 0 };
        point.requests += 1;
        point.inputTokens += item.inputTokens ?? 0;
        point.outputTokens += item.outputTokens ?? 0;
        point.tokens += tokens(item);
        point.costUsd += item.costUsd ?? 0;
        daily.set(day, point);
      }
      return {
        items,
        nextCursor: offset + limit < filtered.length ? `${offset + limit}:mock` : undefined,
        summary: {
          requests: filtered.length,
          inputTokens: filtered.reduce((sum, item) => sum + (item.inputTokens ?? 0), 0),
          outputTokens: filtered.reduce((sum, item) => sum + (item.outputTokens ?? 0), 0),
          cacheReadTokens: filtered.reduce((sum, item) => sum + (item.cacheReadTokens ?? 0), 0),
          cacheWriteTokens: filtered.reduce((sum, item) => sum + (item.cacheWriteTokens ?? 0), 0),
          costUsd: filtered.reduce((sum, item) => sum + (item.costUsd ?? 0), 0),
          protected: filtered.filter((item) => item.verified === true).length,
          blockedLocally: filtered.filter((item) => !item.leftDevice).length,
          failedProof: filtered.filter((item) => item.verified === false).length,
        },
        series: [...daily.entries()]
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([day, point]) => ({ day, ...point })),
        agents: ["claude-code", "codex", "opencode", "pi", "hermes"],
        models: CATALOG.models.map((model) => model.id),
      };
    },
    exportUsageCsv: async () => history.length,
    clearUsage: async () => {
      const count = history.length;
      history = [];
      state = { ...state, activity: [], sessionUsage: usageSummary([]), usageRevision: state.usageRevision + 1 };
      publish();
      return count;
    },
    refreshCatalog: async () => state,
    listAgents: async () => agents,
    disconnectAllAgents: async () => {
      agents = agents.map((agent) => ({ ...agent, connected: false, recorded: false, authorized: false, attention: undefined }));
      return agents;
    },
    previewAgent: async (agentId, connect, options): Promise<AgentPreview> => {
      const agent = agents.find((candidate) => candidate.id === agentId) ?? CLAUDE_OFF;
      if (connect && agent.id === "codex" && !options.defaultModel) {
        throw new Error("Choose a verified default model for Codex");
      }
      return {
        agent,
        connect,
        revision: "mock",
        note: connect
          ? `${agent.name} uses the selected verified model through the local gateway with a machine-local token.`
          : "Only fields written by Private AI Gateway are restored; the agent's local token is revoked.",
        changes: connect
          ? [
              { key: agentId === "codex" ? "model_providers.private_ai_gateway.base_url" : "env.ANTHROPIC_BASE_URL", before: null, after: "http://127.0.0.1:4180", sensitive: false },
              { key: "env.ANTHROPIC_MODEL", before: null, after: options.defaultModel ?? "Discovered at runtime", sensitive: false },
              { key: "apiKeyHelper", before: null, after: "Managed local credential", sensitive: true },
              { key: "env.ANTHROPIC_AUTH_TOKEN", before: "Existing secret", after: null, sensitive: true },
            ]
          : [
              { key: "env.ANTHROPIC_BASE_URL", before: "http://127.0.0.1:4180", after: null, sensitive: false },
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
