import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { DesktopApi, AppState } from "../../shared/contracts";

const key = ["app-state"];

/**
 * The newer of two states. Reads, events and command results arrive in any
 * order; the states of one backend instance carry increasing sequences.
 */
function newer(current: AppState | undefined, next: AppState): AppState {
  return current && current.backendInstance === next.backendInstance && current.sequence > next.sequence ? current : next;
}

/** Rust owns this snapshot; the window keeps the newest one it receives. */
export function useAppState(api: DesktopApi) {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: key,
    queryFn: async () => newer(client.getQueryData(key), await api.getState()),
    retry: false,
  });
  const setState = useCallback((next: AppState) => {
    const current = client.getQueryData<AppState>(key);
    const state = newer(current, next);
    if (state === current) return;
    client.setQueryData(key, state);
    // Usage queries follow persisted usage, not the bounded activity preview.
    if (current && current.usageRevision !== state.usageRevision) {
      for (const queryKey of [["usage"], ["usage-record"], ["usage-receipt"]]) void client.invalidateQueries({ queryKey });
    }
  }, [client]);
  useEffect(() => api.onStateChange(setState), [api, setState]);
  return { ...query, setState };
}
