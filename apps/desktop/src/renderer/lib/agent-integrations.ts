import type { QueryClient } from "@tanstack/react-query";
import type { AgentAccessStatus, AgentStatus, DesktopApi } from "../../shared/contracts";

export type AgentIntegrations = { accessStatus: AgentAccessStatus; agents: AgentStatus[] };
type AccessApi = Pick<DesktopApi, "getAgentAccess" | "requestAgentAccess" | "listAgents">;

// Keep the pre-authorization catalog available without touching Home.
const SUPPORTED_AGENTS = [
  ["claude-code", "Claude Code"],
  ["codex", "Codex"],
  ["hermes", "Hermes Agent"],
  ["pi", "Pi"],
  ["oh-my-pi", "Oh My Pi"],
  ["opencode", "OpenCode"],
  ["openclaw", "OpenClaw"],
] as const;

export function supportedAgentStatuses(): AgentStatus[] {
  return SUPPORTED_AGENTS.map(([id, name]) => ({
    id,
    name,
    configPath: "",
    installed: false,
    connected: false,
    recorded: false,
    authorized: false,
  }));
}

/** Keep the supported catalog visible even when detection returns a partial list. */
export function completeAgentStatuses(agents: AgentStatus[]): AgentStatus[] {
  const detected = new Map(agents.map((agent) => [agent.id, agent]));
  const supported = supportedAgentStatuses();
  const listed = supported.map((agent) => detected.get(agent.id) ?? agent);
  const additional = agents.filter((agent) => !supported.some((entry) => entry.id === agent.id));
  return [...listed, ...additional];
}

/** Only an explicit Enable action may request access; all refreshes are silent. */
export async function readAgentIntegrations(api: AccessApi, requiresAuthorization: boolean, enable = false): Promise<AgentIntegrations> {
  const accessStatus = requiresAuthorization
    ? await (enable ? api.requestAgentAccess() : api.getAgentAccess())
    : "authorized";
  if (accessStatus !== "authorized") return { accessStatus, agents: supportedAgentStatuses() };
  try {
    return { accessStatus, agents: await api.listAgents() };
  } catch (error) {
    // Permission can disappear between the check and the scan.
    if (requiresAuthorization) {
      const current = await api.getAgentAccess();
      if (current !== "authorized") return { accessStatus: current, agents: supportedAgentStatuses() };
    }
    throw error;
  }
}

export function agentIntegrationsLocked(accessStatus: AgentAccessStatus | undefined, pending: boolean): boolean {
  return pending || accessStatus !== "authorized";
}

/** One explicit authorization action, including its scan and cache publication. */
export function createAgentAccessAction(api: AccessApi, requiresAuthorization: boolean, client: QueryClient, onPendingChange: (pending: boolean) => void) {
  let pending = false;
  return {
    get pending() { return pending; },
    async run() {
      if (!requiresAuthorization || pending) return;
      pending = true;
      onPendingChange(true);
      try {
        await client.cancelQueries({ queryKey: ["agents"] });
        const integrations = await readAgentIntegrations(api, requiresAuthorization, true);
        await client.cancelQueries({ queryKey: ["agents"] });
        client.setQueryData<AgentIntegrations>(["agents"], integrations);
      } finally {
        pending = false;
        onPendingChange(false);
      }
    },
  };
}
