import { queryOptions } from "@tanstack/react-query";
import type { UsageQuery } from "../../shared/contracts";
import { desktopApi } from "./environment";
import { USAGE_SEARCH_DEFAULTS, usageDateBounds, usageDateSelection, type UsageSearch } from "./usage-dates";

/** The first page of usage the Usage page's filters select. */
export function usageFilters(search: UsageSearch): UsageQuery {
  const { since, until } = usageDateBounds(usageDateSelection(search));
  return { agent: search.agent, model: search.model, since, until, limit: search.rows ?? USAGE_SEARCH_DEFAULTS.rows };
}

export function usagePageQuery(query: UsageQuery) {
  return queryOptions({
    queryKey: ["usage", query],
    queryFn: () => desktopApi.queryUsage(query),
  });
}

export function cliRegistrationQuery() {
  return queryOptions({
    queryKey: ["cli-registration"],
    queryFn: () => desktopApi.getCliRegistration(),
  });
}
