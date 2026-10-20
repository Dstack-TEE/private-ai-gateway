import type { FetchQueryOptions, QueryClient, QueryKey } from "@tanstack/react-query";

/**
 * Checks for updates now, joining a check already under way: cancelling the
 * query would not stop the shell's command, which runs one update operation at
 * a time, so a second check would fail. A disabled check, as while the channel
 * changes or an update installs, does not run.
 */
export function checkForUpdatesNow(client: QueryClient, queryKey: QueryKey): Promise<void> {
  return client.refetchQueries({ queryKey, exact: true }, { cancelRefetch: false });
}

/**
 * The latest update check before Restart to Update, which disables the
 * query: a fresh check, or the one under way, for the same reason as above.
 */
export function latestUpdateCheck<T, K extends QueryKey>(client: QueryClient, query: FetchQueryOptions<T, Error, T, K>): Promise<T> {
  return client.fetchQuery({ ...query, staleTime: 0 });
}
