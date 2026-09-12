import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { AgentStatus, DesktopApi } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { displayAgentName } from "../lib/agents";

type Options = {
  active: boolean;
  revision?: string;
  verified: boolean;
  onError(message?: string): void;
  notify(message: string): void;
};

/** Query ownership and serialized user intent for native agent connections. */
export function useAgents(api: DesktopApi, { active, revision, verified, onError, notify }: Options) {
  const client = useQueryClient();
  const { data: agents = [], error: agentsError } = useQuery({
    queryKey: ["agents"], queryFn: () => api.listAgents(), staleTime: 0,
    refetchInterval: active ? 15_000 : false,
  });
  useEffect(() => { if (agentsError) onError(errorMessage(agentsError)); }, [agentsError, onError]);
  const [pendingAgentChanges, setPendingAgentChanges] = useState<Record<string, boolean>>({});
  const agentOperations = useRef(new Set<string>());
  const agentIntents = useRef(new Map<string, boolean>());

  const loadAgents = useCallback(async () => {
    try { return await client.fetchQuery({ queryKey: ["agents"], queryFn: () => api.listAgents(), staleTime: 0 }); }
    catch (error) { onError(errorMessage(error)); return undefined; }
  }, [api, client, onError]);
  useEffect(() => { if (active) void loadAgents(); }, [active, loadAgents]);
  useEffect(() => { void loadAgents(); }, [loadAgents, revision, verified]);
  useEffect(() => api.onAgentsChange(() => { void loadAgents(); }), [api, loadAgents]);

  const applyAgent = async (agent: AgentStatus, connect: boolean) => {
    agentIntents.current.set(agent.id, connect);
    setPendingAgentChanges((current) => ({ ...current, [agent.id]: connect }));
    if (agentOperations.current.has(agent.id)) return;
    agentOperations.current.add(agent.id);
    onError(undefined);
    try {
      let changed = agent;
      // Serialize writes per agent and retain the user's latest intent.
      while (agentIntents.current.has(agent.id)) {
        const target = agentIntents.current.get(agent.id);
        if (target === undefined) break;
        if (changed.recorded !== target || (target && !changed.authorized && changed.attention)) {
          const options = {};
          const preview = await api.previewAgent(agent.id, target, options);
          const status = await api.applyAgent(agent.id, target, preview.revision, options);
          changed = status;
          await client.cancelQueries({ queryKey: ["agents"] });
          client.setQueryData<AgentStatus[]>(["agents"], (current) => current?.map((entry) => entry.id === status.id ? status : entry));
        }
        if (agentIntents.current.get(agent.id) === target) {
          agentIntents.current.delete(agent.id);
        }
      }
      notify(`${displayAgentName(agent)} ${changed.recorded ? "connected" : "disconnected"}`);
    } catch (error) {
      onError(errorMessage(error));
    } finally {
      agentIntents.current.delete(agent.id);
      agentOperations.current.delete(agent.id);
      setPendingAgentChanges((current) => { const next = { ...current }; delete next[agent.id]; return next; });
      void loadAgents();
    }
  };


  return { agents, pendingAgentChanges, loadAgents, applyAgent };
}
