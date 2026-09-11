import type {
  AgentPreview,
  AgentStatus,
  CliRegistration,
  ConfidentialProfile,
  ConfidentialProfileInput,
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
  | "backend-disconnected"
  | "configuration-verifying"
  | "ready"
  | "no-profiles"
  | "no-key"
  | "verifying"
  | "error"
  | "empty-catalog"
  | "blocked"
  | "needs-attention"
  | "endpoint-busy"
  | "interactive";

const now = Math.floor(Date.now() / 1000);

const REDPILL_PROFILE: ConfidentialProfile = {
  id: "default",
  name: "RedPill",
  provider: "redpill",
  remoteUrl: "https://tee.redpill.ai",
  auth: { kind: "apiKey" },
  credentialSaved: true,
  verifiedAt: now - 300,
};

const BASE: GatewayState = {
  status: "stopped",
  configurationVerification: false,
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
  config: { remoteUrl: "https://tee.redpill.ai", requireProductionOs: true },
  profiles: [REDPILL_PROFILE],
  activeProfileId: REDPILL_PROFILE.id,
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
  { id: "id-2", section: "9.1(2)", title: "session binding", status: "pass", detail: "Nonce and keyset match the attestation" },
  { id: "id-3", section: "9.1(3)", title: "key validity", status: "pass", detail: "Keyset is current" },
  { id: "id-4", section: "9.1(4)", title: "source provenance", status: "pass", detail: "compose matches" },
  { id: "id-5", section: "9.1(5)", title: "key custody", status: "skip", detail: "Custody policy is not implemented by the CLI" },
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
    case "no-profiles":
      return {
        state: { ...BASE, profiles: [], activeProfileId: "", apiKeySaved: false, remoteUrl: undefined, config: { ...BASE.config, remoteUrl: "" } },
        agents: STOPPED_AGENTS,
      };
    case "no-key":
      return {
        state: { ...BASE, profiles: [{ ...REDPILL_PROFILE, credentialSaved: false, verifiedAt: undefined }], apiKeySaved: false },
        agents: STOPPED_AGENTS,
      };
    case "verifying":
      return {
        state: { ...BASE, status: "verifying", progress: "Reading the verified model list", remoteUrl: BASE.config.remoteUrl, identity: IDENTITY, checks: CHECKS },
        agents: STOPPED_AGENTS,
      };
    case "configuration-verifying":
      return { state: { ...BASE, status: "verifying", configurationVerification: true }, agents: STOPPED_AGENTS };
    case "backend-disconnected":
      return { state: { ...BASE, status: "error", backendConnected: false, endpointError: "Backend disconnected" }, agents: STOPPED_AGENTS };
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
          { ...CODEX, connected: true, recorded: true, authorized: true, attention: "The selected model is not available from this profile. Choose an available model in Codex; the connection does not need to be recreated." },
          { ...CLAUDE, authorized: false, repairAction: "reconnect", attention: "Gateway authentication settings changed. Reconnect this agent, then restart its CLI to reload the configuration." },
          { ...OPENCODE, recorded: true, repairAction: "disconnect", attention: "Disconnect did not complete. Retry Disconnect to restore the configuration." },
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
        state: { ...BASE, profiles: [{ ...REDPILL_PROFILE, credentialSaved: false, verifiedAt: undefined }], apiKeySaved: false, remoteUrl: BASE.config.remoteUrl },
        agents: STOPPED_AGENTS,
      };
  }
}

export function mockApi(name: string | null): DesktopApi {
  const known: MockScenario[] = ["backend-disconnected", "ready", "no-profiles", "no-key", "verifying", "configuration-verifying", "error", "empty-catalog", "blocked", "needs-attention", "endpoint-busy", "interactive"];
  const picked = name?.startsWith("oauth-") ? "no-profiles" : known.find((candidate) => candidate === name) ?? "ready";
  let { state, agents } = scenario(picked);
  if (name === "reconnecting") state = { ...state, status: "stopped", reconnecting: true, protectedSince: now - 600, error: "Network unavailable. Connect to a network; protection resumes after verification." };
  if (name === "all-agent-icons") agents = [...agents,
    { ...PI, id: "oh-my-pi", name: "Oh My Pi", configPath: "/Users/dev/.omp/agent/models.json" },
    { ...OPENCODE, id: "openclaw", name: "OpenClaw", configPath: "/Users/dev/.openclaw/openclaw.json" },
  ];
  if (name === "wake-monitor-unavailable") state.wakeMonitorAvailable = false;
  if (name === "mixed-agents") agents = agents.map((agent) => ({ ...agent, installed: agent.id !== "pi" }));
  if (name === "one-agent") agents = agents.map((agent) => ({ ...agent, installed: agent.id === "codex" }));
  if (name === "no-agents") agents = agents.map((agent) => ({ ...agent, installed: false }));
  if (state.status === "verified" && !state.configurationVerification) state.protectedSince = Math.floor(Date.now() / 1_000) - 600;
  const listeners = new Set<(state: GatewayState) => void>();
  const keyListeners = new Set<(available: boolean) => void>();
  const updateListeners = new Set<(progress: { downloaded: number; total: number }) => void>();
  // Each start gets its own verification run; stop or a newer start makes a
  // pending timer a no-op instead of completing the wrong run.
  let verifyRun = 0;
  let history = [...USAGE_HISTORY];
  const filteredHistory = (query: UsageQuery) => history.filter((item) =>
    (!query.agent || item.agent === query.agent)
    && (!query.model || item.model === query.model)
    && (!query.sessionId || item.sessionId === query.sessionId)
    && (query.since === undefined || item.at >= query.since)
    && (query.until === undefined || item.at < query.until));
  let clientKey = "sk-pap-2f8a19c4d7e6b305a418b62f903c7de84fd119b7a02e65c83b34f09c719a5d2e";
  const credentialProfiles = new Set(state.profiles.filter((profile) => profile.credentialSaved ?? Boolean(profile.verifiedAt)).map((profile) => profile.id));
  const publish = () => {
    const protectedNow = state.status === "verified" && !state.configurationVerification && state.apiKeySaved;
    agents = agents.map((agent) => ({ ...agent, authorized: agent.connected && protectedNow }));
    listeners.forEach((listener) => listener(state));
  };
  const claude = () => agents.find((agent) => agent.id === "claude-code") ?? CLAUDE_OFF;
  let launchPreferences = { openAtLogin: false, connectOnLaunch: false };
  let cliRegistration: CliRegistration = {
    executable: "/Applications/Private AI Proxy.app/Contents/MacOS/pap",
    commandPath: "/Users/dev/.local/bin/pap",
    installed: false,
    onPath: false,
    ...(name === "cli-startup-error" ? {
      startupError: "Command-line registration failed: Move Private AI Proxy to a stable location before registering pap",
    } : {}),
  };
  let updateChannel: "beta" | "stable" = "stable";
  let updateAttempts = 0;
  let keyRotations = 0;
  let failedAccountSave = false;
  let billingRead = name !== "oauth-balance-denied";
  const billingManage = billingRead && name !== "oauth-balance-readonly";
  window.addEventListener("mock:billing-read-granted", () => { billingRead = true; });
  let login: { id: string; profile: ConfidentialProfileInput; polls: number } | undefined;
  return {
    startBackendService: async () => { state = { ...BASE, backendConnected: true }; publish(); return structuredClone(state); },
    showEditMenu: async (editable) => { window.dispatchEvent(new CustomEvent("mock:edit-menu", { detail: { editable } })); },
    getAppearance: async () => {
      if (name === "appearance-pending") await new Promise<void>((resolve) => window.addEventListener("mock:finish-appearance", () => resolve(), { once: true }));
      const value = localStorage.getItem("pap-preview-appearance");
      return value === "light" || value === "dark" ? value : "system";
    },
    setAppearance: async (appearance) => { localStorage.setItem("pap-preview-appearance", appearance); },
    onAppearanceChange: () => () => undefined,
    getAppVersion: async () => "0.1.0",
    getUpdateChannel: async () => updateChannel,
    setUpdateChannel: async (channel) => { updateChannel = channel; return channel; },
    checkUpdate: async () => {
      updateAttempts += 1;
      if (name === "update-offline" || (name === "update-recover" && updateAttempts === 1)) throw new Error("offline");
      return { enabled: true, currentVersion: "0.1.0", channelPublished: name !== "update-unpublished", version: name === "update-available" || name === "update-install-error" ? updateChannel === "beta" ? "0.3.0-beta.1" : "0.2.0" : null };
    },
    installUpdate: async () => {
      if (name === "update-install-error") {
        updateListeners.forEach((listener) => listener({ downloaded: 40, total: 100 }));
        await new Promise<void>((resolve) => window.addEventListener("mock:finish-update", () => resolve(), { once: true }));
        throw new Error("Installation failed");
      }
    },
    onUpdateProgress: (listener) => {
      updateListeners.add(listener);
      listener(name === "update-failed" ? { downloaded: 0, error: "The update could not be downloaded or its signature could not be verified." } : { downloaded: 0 });
      return () => { updateListeners.delete(listener); };
    },
    getLaunchPreferences: async () => launchPreferences,
    setLaunchPreference: async (name, enabled) => {
      launchPreferences = { ...launchPreferences, [name]: enabled };
      return launchPreferences;
    },
    onLaunchPreferencesChange: () => () => undefined,
    getCliRegistration: async () => cliRegistration,
    setCliRegistration: async (installed) => {
      cliRegistration = {
        executable: cliRegistration.executable,
        commandPath: cliRegistration.commandPath,
        installed,
        onPath: installed,
      };
      return cliRegistration;
    },
    onStopAllRequest: () => () => undefined,
    stopAllAndQuit: async () => undefined,
    copyText: async () => undefined,
    getClientKey: async () => {
      if (name === "example-key-pending") await new Promise<void>((resolve) => window.addEventListener("mock:finish-example-key", () => resolve(), { once: true }));
      return clientKey;
    },
    rotateClientKey: async () => {
      keyRotations += 1;
      if (name === "key-rotation-error" && keyRotations === 1) {
        clientKey = "";
        keyListeners.forEach((listener) => listener(false));
        throw new Error("Could not store the replacement client key");
      }
      clientKey = `sk-pap-${Array.from({ length: 4 }, () => Math.random().toString(16).slice(2).padEnd(16, "0")).join("").slice(0, 64)}`;
      keyListeners.forEach((listener) => listener(true));
      return clientKey;
    },
    saveLocalApiConfig: async (config) => {
      if (name === "local-save-pending") await new Promise<void>((resolve) => window.addEventListener("mock:finish-local-save", () => resolve(), { once: true }));
      if (config.port < 1024 || config.port > 65535) throw new Error("Port must be between 1024 and 65535");
      if (!config.allowNetworkAccess && !["127.0.0.1", "::1"].includes(config.listenAddress)) {
        throw new Error("Network listening requires explicit confirmation");
      }
      const host = config.clientHost?.trim() || config.listenAddress;
      const wrapped = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
      state = { ...state, localApi: config, proxyUrl: `http://${wrapped}:${config.port}`, endpointError: undefined };
      publish();
      return state;
    },
    getState: async () => state,
    resetSettings: async () => {
      if (name === "reset-error") throw new Error("Reset could not finish. Retry Reset settings.");
      verifyRun += 1;
      state = { ...state, status: "stopped", sessionActive: false, reconnecting: false, protectedSince: undefined,
        configurationVerification: false, identity: undefined, checks: [], error: undefined,
        config: { ...state.config, requireProductionOs: true },
        localApi: { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 },
        proxyUrl: "http://127.0.0.1:4180" };
      agents = agents.map((agent) => ({ ...agent, connected: false, recorded: false, authorized: false, attention: undefined, repairAction: undefined }));
      launchPreferences = { openAtLogin: false, connectOnLaunch: false };
      updateChannel = "stable";
      localStorage.setItem("pap-preview-appearance", "system");
      for (const key of ["enabled", "gateway", "localApi", "verification"]) localStorage.removeItem(`mock:notifications:${key}`);
      publish();
      window.dispatchEvent(new Event("mock:settings-reset"));
      return state;
    },
    onSettingsReset: (listener) => {
      window.addEventListener("mock:settings-reset", listener);
      return () => window.removeEventListener("mock:settings-reset", listener);
    },
    onStateChange: (listener) => {
      listeners.add(listener);
      const refreshUsage = () => {
        history = history.map((item) => item.inputTokens === undefined ? item : { ...item, inputTokens: item.inputTokens + 1 });
        state = { ...state, usageRevision: (state.usageRevision ?? 0) + 1 };
        publish();
      };
      if (name === "usage-live-refresh") window.addEventListener("mock:refresh-usage", refreshUsage);
      return () => {
        listeners.delete(listener);
        window.removeEventListener("mock:refresh-usage", refreshUsage);
      };
    },
    onNavigate: () => () => undefined,
    onProfileRepairRequest: () => () => undefined,
    onUsageProofRequest: () => () => undefined,
    onClientKeyChange: (listener) => { keyListeners.add(listener); return () => { keyListeners.delete(listener); }; },
    openNativeDialog: async () => undefined,
    closeNativeDialog: async () => undefined,
    nativeDialogReady: async () => { document.documentElement.dataset.nativePresented = "true"; },
    mainWindowReady: async () => { document.documentElement.dataset.mainPresented = "true"; },
    onNativeDialogOpen: (listener) => {
      const open = () => listener({ state: structuredClone(state), repair: false });
      window.addEventListener("mock:dialog-open", open);
      return () => window.removeEventListener("mock:dialog-open", open);
    },
    onNativeDialogDismissed: (listener) => {
      window.addEventListener("mock:dialog-dismissed", listener);
      return () => window.removeEventListener("mock:dialog-dismissed", listener);
    },
    onNativeCloseRequest: (listener) => {
      window.addEventListener("mock:native-close", listener);
      return () => window.removeEventListener("mock:native-close", listener);
    },
    openAboutLink: async () => undefined,
    onAgentsChange: () => () => undefined,
    openAgentWebsite: async () => undefined,
    confirm: async (options) => window.confirm(`${options.title}\n\n${options.message}`),
    start: async (config) => {
      const run = ++verifyRun;
      state = { ...state, status: "verifying", protectedSince: undefined, configurationVerification: false, progress: "Starting the verifier", config, remoteUrl: config.remoteUrl, error: undefined, activity: [], sessionId: `session-mock-${run}`, sessionUsage: usageSummary([]) };
      publish();
      window.setTimeout(() => {
        if (run !== verifyRun || state.status !== "verifying") {
          return;
        }
        // An unreachable service (`*.invalid` is reserved as never-resolving)
        // fails verification like the real backend would.
        state = config.remoteUrl.endsWith(".invalid")
          ? { ...state, status: "error", progress: undefined, error: "The verified gateway did not answer the model list request" }
          : { ...state, status: "verified", protectedSince: Math.floor(Date.now() / 1_000), progress: undefined, identity: IDENTITY, checks: CHECKS, catalog: CATALOG };
        publish();
      }, 350);
      return state;
    },
    saveConfiguration: async (profile, requireProductionOs, key) => {
      const existing = state.profiles.find((entry) => entry.id === profile.id);
      if (!key?.trim() && !credentialProfiles.has(profile.id)) throw new Error("Enter an API key");
      const reconnect = !state.configurationVerification && (state.status === "verified" || state.status === "blocked");
      const saved: ConfidentialProfile = { ...profile, auth: key?.trim() ? { kind: "apiKey" } : existing?.auth ?? { kind: "apiKey" }, credentialSaved: true };
      credentialProfiles.add(profile.id);
      state = { ...state, profiles: [...state.profiles.filter((entry) => entry.id !== profile.id), saved], activeProfileId: profile.id, apiKeySaved: true, status: reconnect ? "verified" : "stopped", configurationVerification: false, config: { remoteUrl: profile.remoteUrl, requireProductionOs } };
      publish();
      return structuredClone(state);
    },
    completeAccountLogin: async (id, callbackUrl) => {
      if (!login || login.id !== id) throw new Error("Account login is no longer active");
      const url = new URL(callbackUrl);
      if (url.origin !== "http://127.0.0.1:4181" || url.pathname !== "/oauth/callback" || !url.searchParams.get("code")) throw new Error("Invalid callback link");
      login.polls = 3;
    },
    beginAccountLogin: async (profile) => {
      login = { id: crypto.randomUUID(), profile, polls: 0 };
      return { id: login.id, url: "https://example.invalid/sign-in", userCode: profile.provider === "phala" ? "ABCD-EFGH" : null };
    },
    pollAccountLogin: async (id) => {
      if (!login || login.id !== id) throw new Error("Account login is no longer active");
      if (name === "oauth-manual-callback" && login.polls < 3) return null;
      if (login.polls++ < 2) return null;
      if (name === "oauth-denied") throw new Error("Authorization was declined");
      return {
        auth: { kind: "oauth", accountId: "preview-account", accountName: "Alice Example", images: { user: "https://img.clerk.com/user-avatar", organization: "https://img.clerk.com/org-avatar" }, scope: { organizationId: login.profile.provider === "redpill" ? "org_test" : null, organization: login.profile.provider === "redpill" ? "Personal organization" : null, workspace: login.profile.provider === "phala" ? "Phala workspace" : null, workspaceId: null } },
        workspaces: login.profile.provider === "redpill" ? [
          { id: 123, name: "Default", isDefault: true },
          ...(name === "oauth-workspaces" ? [{ id: 124, name: "Research", isDefault: false }] : []),
        ] : [],
      };
    },
    saveAccountLogin: async (id, profile, requireProductionOs, workspaceId) => {
      if (!login || login.id !== id || login.polls < 3 || profile.id !== login.profile.id || profile.provider !== login.profile.provider) throw new Error("Finish signing in first");
      if (profile.provider === "redpill" && workspaceId !== 123 && workspaceId !== 124) throw new Error("Choose a workspace before saving");
      if (name === "oauth-save-retry" && !failedAccountSave) {
        failedAccountSave = true;
        throw new Error("Could not store account credential");
      }
      const saved: ConfidentialProfile = { ...profile, auth: { kind: "oauth", accountId: "preview-account", accountName: "Alice Example", images: { user: "https://img.clerk.com/user-avatar", organization: "https://img.clerk.com/org-avatar" }, scope: { organizationId: profile.provider === "redpill" ? "org_test" : null, organization: profile.provider === "redpill" ? "Personal organization" : null, workspace: profile.provider === "redpill" ? workspaceId === 124 ? "Research" : "Default" : "Phala workspace", workspaceId: workspaceId ?? null } }, credentialSaved: true, verifiedAt: Math.floor(Date.now() / 1000) };
      credentialProfiles.add(saved.id);
      state = { ...state, profiles: [...state.profiles.filter((p) => p.id !== saved.id), saved], activeProfileId: saved.id, apiKeySaved: true, status: "stopped", configurationVerification: false, remoteUrl: saved.remoteUrl, config: { remoteUrl: saved.remoteUrl, requireProductionOs } };
      login = undefined;
      publish();
      return structuredClone(state);
    },
    getAccountDetails: async (profileId) => {
      const profile = state.profiles.find((item) => item.id === profileId);
      if (!profile || profile.provider !== "redpill" || profile.auth.kind !== "oauth") throw new Error("Sign in with RedPill to select a workspace");
      const auth = name === "oauth-profile-updated" ? {
        ...profile.auth, accountName: "Alicia Updated",
        images: { user: "https://img.clerk.com/updated-user", organization: "https://img.clerk.com/updated-org" },
        scope: { organizationId: "org_test", organization: "Updated organization", workspace: profile.auth.scope?.workspace ?? null, workspaceId: profile.auth.scope?.workspaceId ?? null },
      } : profile.auth;
      return { auth, workspaces: [{ id: 123, name: "Default", isDefault: true }, { id: 124, name: "Research", isDefault: false }] };
    },
    getAccountBalance: async (target) => {
      if (!billingRead) return null;
      const profile = target.kind === "login" ? login?.id === target.id && login.polls >= 3 ? login.profile : undefined : state.profiles.find((p) => p.id === target.profileId);
      if (!profile) throw new Error("Account is unavailable");
      const saved = target.kind === "profile" ? state.profiles.find((p) => p.id === profile.id) : undefined;
      if (name === "oauth-balance-delayed") await new Promise<void>((resolve) => window.addEventListener("mock:release-balance", () => resolve(), { once: true }));
      return { balanceUsd: "12.50", organizationId: profile.provider === "redpill" ? "org_test" : null, canTopUp: billingManage, grantedUsd: profile.provider === "phala" ? "3.25" : null,
        scope: saved?.auth.kind === "oauth" && saved.auth.scope ? saved.auth.scope : { organization: profile.provider === "redpill" ? "Personal organization" : null, workspace: profile.provider === "phala" ? "Phala workspace" : null, workspaceId: null } };
    },
    openOrganization: async (organizationId) => { window.dispatchEvent(new CustomEvent("mock:manage-organization", { detail: { organizationId } })); },
    openTopUp: async (provider, organizationId) => { window.dispatchEvent(new CustomEvent("mock:top-up", { detail: { provider, organizationId } })); },
    cancelAccountLogin: async (id) => { if (login?.id === id) login = undefined; },
    verifyConfiguration: async (profile, requireProductionOs, key) => {
      const reconnect = !state.configurationVerification && (state.status === "verified" || state.status === "blocked");
      const existing = state.profiles.find((entry) => entry.id === profile.id);
      const profileChanged = !existing
        || existing.provider !== profile.provider
        || existing.remoteUrl.replace(/\/$/, "") !== profile.remoteUrl.replace(/\/$/, "");
      if ((!state.apiKeySaved || profileChanged) && !key?.trim()) {
        throw new Error(profileChanged ? "Enter an API key for this profile" : "Enter an API key");
      }
      const run = ++verifyRun;
      state = { ...state, status: "verifying", configurationVerification: true, progress: "Starting the verifier", remoteUrl: profile.remoteUrl, error: undefined };
      publish();
      await new Promise((resolve) => window.setTimeout(resolve, 350));
      if (run !== verifyRun) throw new Error("Configuration verification was cancelled");
      if (profile.remoteUrl.endsWith(".invalid")) {
        state = { ...state, status: "stopped", configurationVerification: false, progress: undefined };
        publish();
        throw new Error("The verified gateway did not answer the model list request");
      }
      const savedProfile: ConfidentialProfile = {
        ...profile,
        auth: key?.trim() ? { kind: "apiKey" } : existing?.auth ?? { kind: "apiKey" },
        credentialSaved: true,
        verifiedAt: Math.floor(Date.now() / 1000),
      };
      const profiles = existing
        ? state.profiles.map((entry) => entry.id === profile.id ? savedProfile : entry)
        : [...state.profiles, savedProfile];
      credentialProfiles.add(profile.id);
      state = {
        ...state,
        status: reconnect ? "verified" : "stopped",
        protectedSince: reconnect ? Math.floor(Date.now() / 1_000) : undefined,
        configurationVerification: false,
        progress: undefined,
        config: { remoteUrl: profile.remoteUrl, requireProductionOs },
        profiles,
        activeProfileId: profile.id,
        remoteUrl: undefined,
        identity: undefined,
        checks: [],
        catalog: CATALOG,
        apiKeySaved: credentialProfiles.has(profile.id),
      };
      publish();
      return state;
    },
    activateProfile: async (profileId) => {
      const reconnect = !state.configurationVerification && (state.status === "verified" || state.status === "blocked");
      const profile = state.profiles.find((entry) => entry.id === profileId);
      if (!profile) throw new Error("AI service profile not found");
      state = {
        ...state,
        config: { ...state.config, remoteUrl: profile.remoteUrl },
        activeProfileId: profile.id,
        apiKeySaved: profile.credentialSaved ?? Boolean(profile.verifiedAt),
        catalog: undefined,
        status: reconnect ? "verified" : "stopped",
        protectedSince: reconnect ? Math.floor(Date.now() / 1_000) : undefined,
      };
      publish();
      return state;
    },
    deleteProfile: async (profileId) => {
      const profiles = state.profiles.filter((entry) => entry.id !== profileId);
      credentialProfiles.delete(profileId);
      const active = state.activeProfileId === profileId ? profiles[0] : state.profiles.find((entry) => entry.id === state.activeProfileId);
      state = {
        ...state,
        profiles,
        activeProfileId: active?.id ?? "",
        config: { ...state.config, remoteUrl: active?.remoteUrl ?? "" },
        apiKeySaved: active ? credentialProfiles.has(active.id) : false,
        catalog: undefined,
      };
      publish();
      return state;
    },
    stop: async () => {
      if (name === "stop-protection-error") throw new Error("Could not stop protection");
      verifyRun += 1;
      state = { ...state, status: "stopped", sessionActive: false, reconnecting: false, protectedSince: undefined, configurationVerification: false, progress: undefined, identity: undefined, checks: [] };
      publish();
      return state;
    },
    clearApiKey: async () => {
      credentialProfiles.delete(state.activeProfileId);
      state = {
        ...state,
        profiles: state.profiles.map((profile) => profile.id === state.activeProfileId ? { ...profile, credentialSaved: false } : profile),
        apiKeySaved: false,
      };
      publish();
      return state;
    },
    listListenAddresses: async () => {
      if (name === "network-scan-error") throw new Error("Could not read network interfaces. Enter an IP address manually.");
      return [{ address: "127.0.0.1", name: "lo" }, { address: "192.168.1.20", name: "en0" }];
    },
    getNotificationSettings: async () => ({
      preferences: {
        enabled: localStorage.getItem("mock:notifications:enabled") !== "false",
        gateway: localStorage.getItem("mock:notifications:gateway") !== "false",
        localApi: localStorage.getItem("mock:notifications:localApi") !== "false",
        verification: localStorage.getItem("mock:notifications:verification") !== "false",
      },
      permission: name === "notifications-denied" ? "denied" : name === "notifications-prompt" && localStorage.getItem("mock:notifications:permission") !== "granted" ? "notDetermined" : "granted",
      alertsEnabled: name !== "notifications-no-banners",
    }),
    readProfileBackup: async () => ({ version: 1, profiles: [{ name: "Imported Phala", provider: "phala", remoteUrl: "https://inference.phala.com" }] }),
    importProfiles: async (backup) => {
      let imported = 0;
      for (const profile of backup.profiles) {
        if (state.profiles.some((item) => item.name === profile.name && item.provider === profile.provider && item.remoteUrl === profile.remoteUrl)) continue;
        state = { ...state, profiles: [...state.profiles, { ...profile, id: crypto.randomUUID(), auth: { kind: "apiKey" }, credentialSaved: false }] }; imported++;
      }
      publish(); return { imported, skipped: backup.profiles.length - imported };
    },
    exportProfiles: async () => { if (name === "export-error") throw new Error("Could not save the export file"); },
    exportDiagnostics: async () => { if (name === "export-error") throw new Error("Could not save the export file"); },
    saveNotificationSettings: async (config) => {
      if (name === "notification-save-error") throw new Error("Preference unavailable");
      for (const [key, value] of Object.entries(config)) localStorage.setItem(`mock:notifications:${key}`, String(value));
    },
    requestNotificationPermission: async () => { document.documentElement.dataset.notificationPermissionRequested = "true"; localStorage.setItem("mock:notifications:permission", "granted"); return { permission: "granted", alertsEnabled: true }; },
    openNotificationSettings: async () => { document.documentElement.dataset.notificationSettingsOpened = "true"; },
    queryUsage: async (query: UsageQuery) => {
      if (name === "usage-query-pending") await new Promise<void>((resolve) => window.addEventListener("mock:finish-usage-query", () => resolve(), { once: true }));
      if (name === "usage-query-error" && query.model) throw new Error("Usage database temporarily unavailable");
      const filtered = filteredHistory(query);
      const offset = query.cursor ? Number(query.cursor.split(":")[0]) : 0;
      const limit = query.limit ?? 20;
      const items = filtered.slice(offset, offset + limit);
      const tokens = (item: RequestActivity) => (item.inputTokens ?? 0) + (item.outputTokens ?? 0);
      const daily = new Map<string, { requests: number; inputTokens: number; outputTokens: number; tokens: number; costUsd: number }>();
      const byModel = new Map<string, { day: string; model: string | null; requests: number; tokens: number; costUsd: number }>();
      for (const item of filtered) {
        const date = new Date(item.at * 1_000);
        const day = [date.getFullYear(), String(date.getMonth() + 1).padStart(2, "0"), String(date.getDate()).padStart(2, "0")].join("-");
        const point = daily.get(day) ?? { requests: 0, inputTokens: 0, outputTokens: 0, tokens: 0, costUsd: 0 };
        point.requests += 1;
        point.inputTokens += item.inputTokens ?? 0;
        point.outputTokens += item.outputTokens ?? 0;
        point.tokens += tokens(item);
        point.costUsd += item.costUsd ?? 0;
        daily.set(day, point);
        const key = JSON.stringify([day, item.model ?? null]);
        const modelPoint = byModel.get(key) ?? { day, model: item.model ?? null, requests: 0, tokens: 0, costUsd: 0 };
        modelPoint.requests += 1;
        modelPoint.tokens += tokens(item);
        modelPoint.costUsd += item.costUsd ?? 0;
        byModel.set(key, modelPoint);
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
        modelSeries: [...byModel.values()],
        agents: ["claude-code", "codex", "opencode", "pi", "hermes"],
        models: Array.from(new Set(history.flatMap((record) => record.model ? [record.model] : []))),
      };
    },
    getUsageRecord: async (recordId) => {
      const record = history.find((item) => item.id === recordId);
      if (!record) throw new Error("Usage record not found");
      return record;
    },
    exportUsageCsv: async (query) => filteredHistory(query).length,
    clearUsage: async () => {
      const count = history.length;
      history = [];
      state = { ...state, activity: [], sessionUsage: usageSummary([]), usageRevision: state.usageRevision + 1 };
      publish();
      return count;
    },
    refreshCatalog: async () => state,
    listAgents: async () => {
      if (name === "agent-uninstalled" && document.documentElement.dataset.mockAgentRemoved === "true") {
        agents = agents.map((agent) => agent.id === "claude-code" ? { ...agent, installed: false, authorized: false, attention: "CLI not found; previous configuration restored" } : agent);
      }
      return agents;
    },
    disconnectAllAgents: async () => {
      agents = agents.map((agent) => ({ ...agent, connected: false, recorded: false, authorized: false, attention: undefined }));
      return agents;
    },
    previewAgent: async (agentId, connect, options): Promise<AgentPreview> => {
      const agent = agents.find((candidate) => candidate.id === agentId) ?? CLAUDE_OFF;
      return {
        agent,
        connect,
        revision: "mock",
        note: connect
          ? `${agent.name} uses the selected verified model through the local gateway with a machine-local token.`
          : "Only fields written by Private AI Proxy are restored; the agent's local token is revoked.",
        changes: connect
          ? [
              { key: agentId === "codex" ? "model_providers.private_ai_proxy.base_url" : "env.ANTHROPIC_BASE_URL", before: null, after: "http://127.0.0.1:4180", sensitive: false },
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
      if (name === "agent-pending") {
        window.dispatchEvent(new Event("mock:agent-write"));
        if (connect) await new Promise<void>((resolve) => window.addEventListener("mock:finish-agent", () => resolve(), { once: true }));
      }
      agents = agents.map((agent) =>
        agent.id === agentId
          ? { ...agent, connected: connect, recorded: connect, authorized: connect && state.status === "verified" && !state.configurationVerification && state.apiKeySaved, attention: undefined }
          : agent,
      );
      return agents.find((agent) => agent.id === agentId) ?? claude();
    },
  };
}
