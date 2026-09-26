import { useEffect, useMemo } from "react";
import { keepPreviousData, useIsMutating, useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { AgentStatus, AppState, DesktopApi } from "../../shared/contracts";
import { errorMessage, toastError } from "../lib/error-message";
import { agentIntegrationsLocked, completeAgentStatuses, readAgentIntegrations, type AgentIntegrations } from "../lib/agent-integrations";

const connectionKey = (agentId: string) => ["agent-connection", agentId];

/**
 * The agents the backend detects. They change with the backend's
 * `agentsRevision` and verified catalog; returning to the window rescans for
 * agents installed meanwhile.
 */
export function useAgents(api: DesktopApi, state: AppState, requiresAuthorization: boolean) {
  const client = useQueryClient();
  // Only this explicit action requests access; its scan publishes the result.
  const access = useMutation({
    mutationFn: async () => {
      await client.cancelQueries({ queryKey: ["agents"] });
      return readAgentIntegrations(api, requiresAuthorization, true);
    },
    onSuccess: async (integrations) => {
      await client.cancelQueries({ queryKey: ["agents"] });
      client.setQueriesData<AgentIntegrations>({ queryKey: ["agents"] }, integrations);
    },
    onError: (failure) => toastError("Could not grant agent access", failure),
  });
  const authorizing = access.isPending;
  const { data, error } = useQuery({
    queryKey: ["agents", state.backendInstance, state.agentsRevision, state.catalog?.revision, state.protection.phase === "protected"],
    queryFn: () => readAgentIntegrations(api, requiresAuthorization),
    enabled: !authorizing,
    placeholderData: keepPreviousData,
  });
  useEffect(() => api.onAgentsChange(() => { void client.invalidateQueries({ queryKey: ["agents"] }); }), [api, client]);
  const accessStatus = requiresAuthorization ? data?.accessStatus : "authorized";
  const changing = useIsMutating({ mutationKey: ["agent-connection"] }) > 0;
  const { mutate: requestAccess } = access;
  return useMemo(() => ({
    agents: completeAgentStatuses(data?.agents ?? []),
    accessStatus,
    authorizing,
    /** An agent connection is changing. */
    changing,
    controlsLocked: agentIntegrationsLocked(accessStatus, authorizing),
    problem: error ? errorMessage(error) : undefined,
    requestAccess: () => {
      if (requiresAuthorization && !authorizing) requestAccess();
    },
  }), [data, error, accessStatus, authorizing, changing, requiresAuthorization, requestAccess]);
}

/**
 * Connects or disconnects one agent. Changes of the same agent run one after
 * another (the mutation scope), each with the backend's own preview.
 */
export function useAgentConnection(api: DesktopApi, agent: AgentStatus) {
  const client = useQueryClient();
  const mutation = useMutation({
    mutationKey: connectionKey(agent.id),
    scope: { id: `agent-connection:${agent.id}` },
    mutationFn: (connect: boolean) => api.setAgentConnection(agent.id, connect),
    onSuccess: (status) => {
      client.setQueriesData<AgentIntegrations>({ queryKey: ["agents"] }, (current) => current && ({
        ...current,
        agents: current.agents.map((entry) => entry.id === status.id ? status : entry),
      }));
      toast.success(`${agent.name} ${status.recorded ? "connected" : "disconnected"}`);
    },
    onError: (failure, connect) => toastError(`${agent.name} could not ${connect ? "connect" : "disconnect"}`, failure),
    onSettled: () => client.invalidateQueries({ queryKey: ["agents"] }),
  });
  const pending = useMutationState({
    filters: { mutationKey: connectionKey(agent.id), status: "pending" },
    select: (entry) => entry.state.variables,
  }).at(-1);
  return {
    /** The connection the latest pending change asks for. */
    pending: typeof pending === "boolean" ? pending : undefined,
    change: (connect: boolean) => mutation.mutate(connect),
  };
}
