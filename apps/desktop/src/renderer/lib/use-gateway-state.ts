import { useCallback, useEffect, useRef, type SetStateAction } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { DesktopApi, GatewayState } from "../../shared/contracts";

const key = ["gateway-state"];

/** Rust owns this snapshot. Events and command results supersede pending reads. */
export function useGatewayState(api: DesktopApi, fallback: GatewayState) {
  const client = useQueryClient();
  const query = useQuery({ queryKey: key, queryFn: () => api.getState(), retry: false });
  const usageRevision = useRef(query.data?.usageRevision);
  useEffect(() => {
    if (usageRevision.current === query.data?.usageRevision) return;
    usageRevision.current = query.data?.usageRevision;
    void client.invalidateQueries({ queryKey: ["usage"] });
    void client.invalidateQueries({ queryKey: ["usage-record"] });
  }, [client, query.data?.usageRevision]);
  const setState = useCallback((next: SetStateAction<GatewayState>) => {
    void client.cancelQueries({ queryKey: key }).then(() => {
      client.setQueryData<GatewayState>(key, (current) => typeof next === "function" ? next(current ?? fallback) : next);
    });
  }, [client, fallback]);
  useEffect(() => api.onStateChange(setState), [api, setState]);
  return { ...query, setState };
}
