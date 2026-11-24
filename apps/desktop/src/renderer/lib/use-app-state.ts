import { useCallback, useEffect } from "react";
import { QueryObserver, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { DesktopApi, AppState } from "../../shared/contracts";

export const appStateKey = ["app-state"];

/**
 * The newer of two states. Reads, events and command results arrive in any
 * order; the states of one backend instance carry increasing sequences. A
 * disconnected state keeps the last sequence it saw, so it always applies.
 */
export function newerState(current: AppState | undefined, next: AppState): AppState {
  return current && next.backendConnected !== false && current.backendInstance === next.backendInstance
    && current.sequence > next.sequence ? current : next;
}

/**
 * Reads the state. An event or command result that arrives during the read
 * may be newer than the reply, so the reply applies only through
 * `newerState`. A state from another backend that arrives during the read
 * means that backend replaced the one that replied; it is kept, and its
 * events carry later changes.
 */
export async function readState(client: QueryClient, api: Pick<DesktopApi, "getState">): Promise<AppState> {
  const before = client.getQueryData<AppState>(appStateKey);
  const reply = await api.getState();
  const current = client.getQueryData<AppState>(appStateKey);
  if (current && current !== before && current.backendInstance !== reply.backendInstance) return current;
  return newerState(current, reply);
}

/** Resolves once protection is not verifying; saving a profile waits for it. */
export function afterVerification(client: QueryClient): Promise<void> {
  return new Promise((resolve) => {
    const observer = new QueryObserver<AppState>(client, { queryKey: appStateKey, enabled: false });
    const settle = ({ data }: { data?: AppState }) => {
      if (data?.status === "verifying") return;
      unsubscribe();
      resolve();
    };
    const unsubscribe = observer.subscribe(settle);
    settle(observer.getCurrentResult());
  });
}

/** Rust owns this snapshot; the window keeps the newest one it receives. */
export function useAppState(api: DesktopApi) {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: appStateKey,
    queryFn: () => readState(client, api),
    retry: false,
  });
  const setState = useSetAppState();
  useEffect(() => api.onStateChange(setState), [api, setState]);
  return { ...query, setState };
}

/** Applies a state a command returned, unless the window has a newer one. */
export function useSetAppState() {
  const client = useQueryClient();
  return useCallback((next: AppState) => {
    const current = client.getQueryData<AppState>(appStateKey);
    const state = newerState(current, next);
    if (state === current) return;
    client.setQueryData(appStateKey, state);
    // Usage queries follow persisted usage, not the bounded activity preview.
    if (current && current.usageRevision !== state.usageRevision) {
      for (const queryKey of [["usage"], ["usage-record"], ["usage-receipt"]]) void client.invalidateQueries({ queryKey });
    }
    // A backend that answers starts the `pap` command's automatic
    // registration; a read made before it did is stale.
    if (current && state.backendConnected !== false && (current.backendConnected === false || current.backendInstance !== state.backendInstance)) {
      void client.invalidateQueries({ queryKey: ["cli-registration"] });
    }
  }, [client]);
}
