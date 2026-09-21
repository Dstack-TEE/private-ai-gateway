import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { AgentStatus, DesktopApi } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { agentIntegrationsLocked, completeAgentStatuses, createAgentAccessAction, readAgentIntegrations, type AgentIntegrations } from "../lib/agent-integrations";
import { showErrorAlert } from "../lib/error-alert";

type Options = {
  requiresAuthorization: boolean;
  active: boolean;
  revision?: string;
  verified: boolean;
  notify(message: string): void;
};

/** Query ownership and serialized user intent for native agent connections. */
export function useAgents(api: DesktopApi, { requiresAuthorization, active, revision, verified, notify }: Options) {
  const client = useQueryClient();
  const readIntegrations = useCallback(() => readAgentIntegrations(api, requiresAuthorization), [api, requiresAuthorization]);
  const [authorizing, setAuthorizing] = useState(false);
  const accessAction = useMemo(() => createAgentAccessAction(api, requiresAuthorization, client, setAuthorizing), [api, requiresAuthorization, client]);
  const { data, error: agentsError } = useQuery({
    queryKey: ["agents"], queryFn: readIntegrations, staleTime: 0,
    enabled: !authorizing,
    refetchInterval: active ? 15_000 : false,
  });
  const accessStatus = requiresAuthorization ? data?.accessStatus : "authorized";
  const controlsLocked = agentIntegrationsLocked(accessStatus, authorizing);
  const agents = agentsError ? [] : completeAgentStatuses(data?.agents ?? []);
  const [pendingAgentChanges, setPendingAgentChanges] = useState<Record<string, boolean>>({});
  const agentOperations = useRef(new Set<string>());
  const agentIntents = useRef(new Map<string, boolean>());

  const loadAgents = useCallback(async () => {
    if (accessAction.pending) return undefined;
    try {
      return (await client.fetchQuery({ queryKey: ["agents"], queryFn: readIntegrations, staleTime: 0 })).agents;
    } catch {
      return undefined;
    }
  }, [accessAction, client, readIntegrations]);
  useEffect(() => { if (active) void loadAgents(); }, [active, loadAgents]);
  useEffect(() => { void loadAgents(); }, [loadAgents, revision, verified]);
  useEffect(() => api.onAgentsChange(() => { void loadAgents(); }), [api, loadAgents]);

  const requestAccess = async () => {
    try {
      await accessAction.run();
    } catch (error) {
      await showErrorAlert("Agent access could not be granted", errorMessage(error), api);
    }
  };

  const applyAgent = async (agent: AgentStatus, connect: boolean) => {
    if (agentIntegrationsLocked(requiresAuthorization ? client.getQueryData<AgentIntegrations>(["agents"])?.accessStatus : "authorized", accessAction.pending)) return;
    agentIntents.current.set(agent.id, connect);
    setPendingAgentChanges((current) => ({ ...current, [agent.id]: connect }));
    if (agentOperations.current.has(agent.id)) return;
    agentOperations.current.add(agent.id);
    let failure: string | undefined;
    let attemptedConnection = connect;
    try {
      let changed = agent;
      // Serialize writes per agent and retain the user's latest intent.
      while (agentIntents.current.has(agent.id)) {
        const target = agentIntents.current.get(agent.id);
        if (target === undefined) break;
        attemptedConnection = target;
        if (changed.recorded !== target || (target && !changed.authorized && changed.attention)) {
          const options = {};
          const preview = await api.previewAgent(agent.id, target, options);
          const status = await api.applyAgent(agent.id, target, preview.revision, options);
          changed = status;
          await client.cancelQueries({ queryKey: ["agents"] });
          client.setQueryData<AgentIntegrations>(["agents"], (current) => current && ({ ...current, agents: current.agents.map((entry) => entry.id === status.id ? status : entry) }));
        }
        if (agentIntents.current.get(agent.id) === target) {
          agentIntents.current.delete(agent.id);
        }
      }
      notify(`${agent.name} ${changed.recorded ? "connected" : "disconnected"}`);
    } catch (error) {
      failure = errorMessage(error);
    } finally {
      agentIntents.current.delete(agent.id);
      agentOperations.current.delete(agent.id);
      setPendingAgentChanges((current) => { const next = { ...current }; delete next[agent.id]; return next; });
      void loadAgents();
    }
    if (failure) {
      await showErrorAlert(`${agent.name} could not ${attemptedConnection ? "connect" : "disconnect"}`, failure, api);
    }
  };
  const problem = agentsError ? errorMessage(agentsError) : undefined;
  return { agents, accessStatus, authorizing, controlsLocked, pendingAgentChanges, loadAgents, requestAccess, applyAgent, problem };
}
