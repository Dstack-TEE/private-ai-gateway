import type { QueryClient, QueryKey } from "@tanstack/react-query";

/**
 * Checks for updates now, joining a check already under way: cancelling the
 * query would not stop the shell's command, which runs one update operation at
 * a time, so a second check would fail. A disabled check, as while the channel
 * changes or an update installs, does not run.
 */
export function checkForUpdatesNow(client: QueryClient, queryKey: QueryKey): Promise<void> {
  return client.refetchQueries({ queryKey, exact: true }, { cancelRefetch: false });
}
