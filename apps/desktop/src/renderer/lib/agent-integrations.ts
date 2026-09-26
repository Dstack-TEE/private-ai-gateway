// Node runs `npm run test:agents` on this file as is, so a value import names its file.
import { AGENTS } from "../../shared/contracts.generated.ts";
import type { AgentAccessStatus, AgentStatus, DesktopApi } from "../../shared/contracts";

export type AgentIntegrations = { accessStatus: AgentAccessStatus; agents: AgentStatus[] };
type AccessApi = Pick<DesktopApi, "getAgentAccess" | "requestAgentAccess" | "listAgents">;

/** The supported agents, listed before authorization without touching Home. */
export function supportedAgentStatuses(): AgentStatus[] {
  return AGENTS.map(({ id, name }) => ({
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
