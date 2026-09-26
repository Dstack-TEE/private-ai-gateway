import { useCallback, useEffect } from "react";
import { QueryObserver, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { DesktopApi, AppState } from "../../shared/contracts";

const key = ["app-state"];

/**
 * The newer of two states. Reads, events and command results arrive in any
 * order; the states of one backend instance carry increasing sequences. A
 * disconnected state keeps the last sequence it saw, so it always applies.
 */
export function newerState(current: AppState | undefined, next: AppState): AppState {
  return current && next.backendConnected !== false && current.backendInstance === next.backendInstance
    && current.sequence > next.sequence ? current : next;
}

/** Resolves once protection is not verifying; saving a profile waits for it. */
export function afterVerification(client: QueryClient): Promise<void> {
  return new Promise((resolve) => {
    const observer = new QueryObserver<AppState>(client, { queryKey: key, enabled: false });
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
    queryKey: key,
    queryFn: async () => newerState(client.getQueryData(key), await api.getState()),
    retry: false,
  });
  const setState = useCallback((next: AppState) => {
    const current = client.getQueryData<AppState>(key);
    const state = newerState(current, next);
    if (state === current) return;
    client.setQueryData(key, state);
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
  useEffect(() => api.onStateChange(setState), [api, setState]);
  return { ...query, setState };
}
