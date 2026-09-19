import type { QueryClient } from "@tanstack/react-query";
import type { AgentAccessStatus, AgentStatus, DesktopApi } from "../../shared/contracts";

export type AgentIntegrations = { accessStatus: AgentAccessStatus; agents: AgentStatus[] };
type AccessApi = Pick<DesktopApi, "getAgentAccess" | "requestAgentAccess" | "listAgents">;

/** Only an explicit Enable action may request access; all refreshes are silent. */
export async function readAgentIntegrations(api: AccessApi, requiresAuthorization: boolean, enable = false): Promise<AgentIntegrations> {
  const accessStatus = requiresAuthorization
    ? await (enable ? api.requestAgentAccess() : api.getAgentAccess())
    : "authorized";
  if (accessStatus !== "authorized") return { accessStatus, agents: [] };
  try {
    return { accessStatus, agents: await api.listAgents() };
  } catch (error) {
    // Permission can disappear between the check and the scan.
    if (requiresAuthorization) {
      const current = await api.getAgentAccess();
      if (current !== "authorized") return { accessStatus: current, agents: [] };
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
