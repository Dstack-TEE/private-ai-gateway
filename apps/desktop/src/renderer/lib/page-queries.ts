import { queryOptions } from "@tanstack/react-query";
import type { UsageQuery } from "../../shared/contracts";
import { desktopApi } from "./environment";
import { usageDateBounds } from "./usage-dates";

export function usagePageQuery(query?: UsageQuery) {
  const { since, until } = usageDateBounds({ preset: "7d" });
  const request = query ?? { since, until, limit: 20 };
  return queryOptions({
    queryKey: ["usage", request],
    queryFn: () => desktopApi.queryUsage(request),
  });
}

export function cliRegistrationQuery() {
  return queryOptions({
    queryKey: ["cli-registration"],
    queryFn: () => desktopApi.getCliRegistration(),
  });
}
