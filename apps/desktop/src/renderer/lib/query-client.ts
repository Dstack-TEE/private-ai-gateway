import { focusManager, QueryClient } from "@tanstack/react-query";
import { windowFocus } from "./environment";

// Commands use local IPC even when the browser reports the network as offline.
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: { networkMode: "always", refetchOnReconnect: true, staleTime: 2_000, gcTime: 5 * 60_000, retry: 2 },
    mutations: { networkMode: "always", retry: false },
  },
});

declare module "@tanstack/react-query" {
  interface Register {
    /** `errorTitle` opts a read or an action in to reporting its failure (`ConfirmProvider`). */
    queryMeta: { errorTitle?: string };
    mutationMeta: { errorTitle?: string };
  }
}

// Queries refetch when the desktop window becomes active, as a browser tab
// does when it becomes visible.
if (windowFocus) focusManager.setEventListener(windowFocus);
