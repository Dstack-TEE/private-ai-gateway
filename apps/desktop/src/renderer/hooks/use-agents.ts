import { useEffect, useRef } from "react";
import { keepPreviousData, useIsMutating, useMutation, useMutationState, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AgentStatus, AppState } from "../../shared/contracts";
import { useConfirm, useReportFailure } from "../components/confirm";
import { agentAccessMutation, agentIntegrationsLocked, completeAgentStatuses, readAgentIntegrations, type AgentIntegrations } from "../lib/agent-integrations";
import { desktopApi, distributionCapabilities } from "../lib/environment";
import { useShell } from "../lib/shell";

const connectionKey = (agentId: string) => ["agent-connection", agentId];
/** The agent whose background service keeps the settings it started with. */
export const SERVICE_AGENT = "codex";
/** A sandboxed app reads Home only once the user grants access. */
const requiresAuthorization = distributionCapabilities.sandboxHomeAccess;

/**
 * The agents the backend detects. They change with the backend's
 * `agentsRevision` and verified catalog; returning to the window rescans for
 * agents installed meanwhile.
 *
 * Codex's background service keeps the settings it started with, so after a
 * Codex change, from a row or the tray, `offerServiceStop` offers to stop it.
 * That ends running Codex sessions, so it asks first; the next Codex run
 * starts it again with the new settings.
 */
export function useAgents(state: AppState, backendReady: boolean) {
  const client = useQueryClient();
  const confirm = useConfirm();
  const { mutate: stopService } = useMutation({
    mutationFn: () => desktopApi.stopAgentService(SERVICE_AGENT),
    meta: { errorTitle: "Could not stop Codex's background service" },
  });
  // One question at a time: a change made while it asks doesn't ask again.
  const offeringServiceStop = useRef(false);
  const offerServiceStop = async (agentId: string) => {
    if (agentId !== SERVICE_AGENT || offeringServiceStop.current) return;
    offeringServiceStop.current = true;
    try {
      // A service that can't be checked counts as not running, as in the backend.
      if (!await desktopApi.agentServiceRunning(agentId).catch(() => false)) return;
      if (await confirm({
        title: "Restart Codex to apply?",
        message: "Codex's background service still has the previous settings. Stopping it ends running Codex sessions; it starts again the next time you open Codex.",
        confirmLabel: "Stop Codex Service",
        cancelLabel: "Later",
        destructive: true,
      })) stopService();
    } finally {
      offeringServiceStop.current = false;
    }
  };
  const access = useMutation({
    ...agentAccessMutation(desktopApi, requiresAuthorization, client),
    meta: { errorTitle: "Could not grant agent access" },
  });
  const authorizing = access.isPending;
  // Nothing is read until the backend answers. While it is unavailable,
  // which the window shows, the agents stay locked.
  const backendUnavailable = state.backendConnected === false;
  const { data, error } = useQuery({
    queryKey: ["agents", state.backendInstance, state.agentsRevision, state.catalog?.revision, state.protection.phase === "protected"],
    queryFn: () => readAgentIntegrations(desktopApi, requiresAuthorization),
    enabled: !authorizing && backendReady,
    placeholderData: keepPreviousData,
    meta: { errorTitle: "Could not detect agents" },
  });
  useEffect(() => desktopApi.onAgentsChange(() => { void client.invalidateQueries({ queryKey: ["agents"] }); }), [client]);
  const accessStatus = requiresAuthorization ? data?.accessStatus : "authorized";
  const changing = useIsMutating({ mutationKey: ["agent-connection"] }) > 0;
  return {
    agents: completeAgentStatuses(data?.agents ?? []),
    accessStatus,
    authorizing,
    /** Why the agents can't be listed yet: access is being checked, required or granted. */
    detectionLabel: accessStatus === "authorized" ? undefined : authorizing ? "Waiting for access" : accessStatus ? "Access required" : "Checking access",
    /** An agent connection is changing. */
    changing,
    controlsLocked: agentIntegrationsLocked(accessStatus, authorizing) || backendUnavailable,
    /** The agents can't be read: the read failed, or the backend is unavailable. */
    problem: Boolean(error) || backendUnavailable,
    requestAccess: () => {
      if (requiresAuthorization && !authorizing) access.mutate();
    },
    offerServiceStop,
  };
}

/**
 * Connects or disconnects one agent. Changes of the same agent run one after
 * another (the mutation scope), each with the backend's own preview; after
 * each, `offerServiceStop` (from `useAgents`) may offer to stop its service.
 */
export function useAgentConnection(agent: AgentStatus) {
  const client = useQueryClient();
  const reportFailure = useReportFailure();
  const { agents: { offerServiceStop } } = useShell();
  const mutation = useMutation({
    mutationKey: connectionKey(agent.id),
    scope: { id: `agent-connection:${agent.id}` },
    mutationFn: (connect: boolean) => desktopApi.setAgentConnection(agent.id, connect),
    onSuccess: (status) => {
      client.setQueriesData<AgentIntegrations>({ queryKey: ["agents"] }, (current) => current && ({
        ...current,
        agents: current.agents.map((entry) => entry.id === status.id ? status : entry),
      }));
      void offerServiceStop(agent.id);
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
