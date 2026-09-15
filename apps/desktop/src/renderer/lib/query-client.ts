import { focusManager, QueryClient } from "@tanstack/react-query";

// Commands use local IPC even when the browser reports the network as offline.
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: { networkMode: "always", refetchOnReconnect: true, staleTime: 2_000, gcTime: 5 * 60_000, retry: 2 },
    mutations: { networkMode: "always", retry: false },
  },
});

// Webview window activation also needs revalidation, not just tab visibility.
focusManager.setEventListener((onFocus) => {
  const refresh = () => onFocus();
  window.addEventListener("focus", refresh);
  document.addEventListener("visibilitychange", refresh);
  return () => {
    window.removeEventListener("focus", refresh);
    document.removeEventListener("visibilitychange", refresh);
  };
});
