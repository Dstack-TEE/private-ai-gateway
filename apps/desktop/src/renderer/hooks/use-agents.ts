import { useEffect, useMemo } from "react";
import { keepPreviousData, useIsMutating, useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AgentStatus, AppState, DesktopApi } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { useConfirm, useReportFailure } from "../components/confirm";
import { agentAccessMutation, agentIntegrationsLocked, completeAgentStatuses, readAgentIntegrations, type AgentIntegrations } from "../lib/agent-integrations";

const connectionKey = (agentId: string) => ["agent-connection", agentId];
/** The agent whose background service keeps the settings it started with. */
const SERVICE_AGENT = "codex";

/**
 * The agents the backend detects. They change with the backend's
 * `agentsRevision` and verified catalog; returning to the window rescans for
 * agents installed meanwhile.
 */
export function useAgents(api: DesktopApi, state: AppState, requiresAuthorization: boolean) {
  const client = useQueryClient();
  const reportFailure = useReportFailure();
  const access = useMutation({
    ...agentAccessMutation(api, requiresAuthorization, client),
    onError: (failure) => reportFailure("Could not grant agent access", failure),
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
 *
 * Codex's background service keeps the settings it started with, so after a
 * change it offers to stop it; that ends running Codex sessions, so it asks
 * first, and the next Codex run starts it again with the new settings.
 */
export function useAgentConnection(api: DesktopApi, agent: AgentStatus) {
  const client = useQueryClient();
  const confirm = useConfirm();
  const reportFailure = useReportFailure();
  const { mutate: stopService } = useMutation({
    mutationFn: () => api.stopAgentService(agent.id),
    onError: (failure) => reportFailure("Could not stop Codex's background service", failure),
  });
  const offerServiceStop = async () => {
    // A service that can't be checked counts as not running, as in the backend.
    if (!await api.agentServiceRunning(agent.id).catch(() => false)) return;
    try {
      if (await confirm({
        title: "Restart Codex to apply?",
        message: "Codex's background service still has the previous settings. Stopping it ends running Codex sessions; it starts again the next time you open Codex.",
        confirmLabel: "Stop Codex Service",
        cancelLabel: "Later",
        destructive: true,
      })) stopService();
    } catch (error) {
      reportFailure("Could not stop Codex's background service", error);
    }
  };
  const mutation = useMutation({
    mutationKey: connectionKey(agent.id),
    scope: { id: `agent-connection:${agent.id}` },
    mutationFn: (connect: boolean) => api.setAgentConnection(agent.id, connect),
    onSuccess: (status) => {
      client.setQueriesData<AgentIntegrations>({ queryKey: ["agents"] }, (current) => current && ({
        ...current,
        agents: current.agents.map((entry) => entry.id === status.id ? status : entry),
      }));
      if (agent.id === SERVICE_AGENT) void offerServiceStop();
    },
    onError: (failure, connect) => reportFailure(`${agent.name} could not ${connect ? "connect" : "disconnect"}`, failure),
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
