import { createBrowserHistory, createMemoryHistory, createRootRoute, createRoute, createRouter, redirect, stripSearchParams } from "@tanstack/react-router";
import { AppLayout } from "./app";
import { SignInPage } from "./components/sign-in";
import { session, web } from "./lib/environment";
import { queryClient } from "./lib/query-client";
import { USAGE_SEARCH_DEFAULTS, validateUsageSearch } from "./lib/usage-dates";

const rootRoute = createRootRoute();

/** Only a same-origin path may be the page to return to after sign-in. */
function returnPath(value: unknown): string | undefined {
  return typeof value === "string" && value.startsWith("/") && !value.startsWith("//") ? value : undefined;
}

// The web UI signs in first; the desktop window has no session.
const signInRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/sign-in",
  validateSearch: (search: Record<string, unknown>): { redirect?: string } => ({ redirect: returnPath(search.redirect) }),
  component: SignInPage,
});

// Every page needs a session in the web UI (TanStack Router authenticated
// routes). AppLayout renders the page matching the location.
const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "_app",
  beforeLoad: async ({ location }) => {
    if (!session) return;
    let live = false;
    let notice: string | undefined;
    try {
      live = await session.check();
    } catch {
      notice = "Cannot reach Private AI Proxy. Check that it is running, then sign in.";
    }
    if (!live) throw redirect({ to: "/sign-in", search: { redirect: location.href }, state: { notice }, replace: true });
  },
  component: AppLayout,
});
const overviewRoute = createRoute({ getParentRoute: () => appRoute, path: "/" });
const agentsRoute = createRoute({ getParentRoute: () => appRoute, path: "/agents" });
const usageRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/usage",
  validateSearch: validateUsageSearch,
  search: { middlewares: [stripSearchParams(USAGE_SEARCH_DEFAULTS)] },
});
const settingsRoute = createRoute({ getParentRoute: () => appRoute, path: "/settings" });
const unknownRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "$",
  beforeLoad: () => {
    throw redirect({ to: "/", replace: true });
  },
});

export const router = createRouter({
  routeTree: rootRoute.addChildren([
    signInRoute,
    appRoute.addChildren([overviewRoute, agentsRoute, usageRoute, settingsRoute, unknownRoute]),
  ]),
  // The web UI has real URLs. The desktop window has no address bar or deep
  // links, so its location stays in memory and the webview keeps loading index.html.
  history: web ? createBrowserHistory() : createMemoryHistory(),
});

// A session that ends, here or on the server, returns to sign-in; nothing
// cached for it survives.
session?.onEnded((notice) => {
  queryClient.clear();
  const location = router.state.location;
  void router.navigate({
    to: "/sign-in",
    search: { redirect: location.pathname === "/sign-in" ? undefined : location.href },
    state: { notice },
    replace: true,
  });
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
