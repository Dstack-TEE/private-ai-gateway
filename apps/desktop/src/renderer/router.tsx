import { createBrowserHistory, createMemoryHistory, createRootRoute, createRoute, createRouter, redirect, stripSearchParams } from "@tanstack/react-router";
import { App } from "./app";
import { web } from "./lib/environment";
import { USAGE_SEARCH_DEFAULTS, validateUsageSearch } from "./lib/usage-dates";

// App is the layout for every page and renders the one matching the location.
const rootRoute = createRootRoute({ component: App });
const overviewRoute = createRoute({ getParentRoute: () => rootRoute, path: "/" });
const agentsRoute = createRoute({ getParentRoute: () => rootRoute, path: "/agents" });
const usageRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/usage",
  validateSearch: validateUsageSearch,
  search: { middlewares: [stripSearchParams(USAGE_SEARCH_DEFAULTS)] },
});
const settingsRoute = createRoute({ getParentRoute: () => rootRoute, path: "/settings" });
const unknownRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "$",
  beforeLoad: () => {
    throw redirect({ to: "/", replace: true });
  },
});

export const router = createRouter({
  routeTree: rootRoute.addChildren([overviewRoute, agentsRoute, usageRoute, settingsRoute, unknownRoute]),
  // The web UI has real URLs. The desktop window has no address bar or deep
  // links, so its location stays in memory and the webview keeps loading index.html.
  history: web ? createBrowserHistory() : createMemoryHistory(),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
  interface HistoryState {
    /** Announced once the page it navigates to is shown. */
    notice?: string;
  }
}
