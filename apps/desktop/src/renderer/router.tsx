import type { QueryClient } from "@tanstack/react-query";
import { createBrowserHistory, createMemoryHistory, createRootRouteWithContext, createRoute, createRouter, redirect, stripSearchParams } from "@tanstack/react-router";
import { Bot, ChartNoAxesColumn, LayoutGrid, Settings, type LucideIcon } from "lucide-react";
import { AppLayout } from "./app";
import { PageError, PageNotFound, WindowError } from "./components/route-error";
import { SignInPage } from "./components/sign-in";
import { AgentsPage } from "./features/agents";
import { OverviewPage } from "./features/overview";
import { SettingsPage } from "./features/settings";
import { UsagePage } from "./features/usage";
import { distributionCapabilities, session, web } from "./lib/environment";
import { cliRegistrationQuery, usageFilters, usagePageQuery } from "./lib/page-queries";
import { queryClient } from "./lib/query-client";
import { USAGE_SEARCH_DEFAULTS, validateUsageSearch } from "./lib/usage-dates";

// Loaders start the queries their page reads without awaiting them (TanStack
// Query's router integration), so navigation never waits; the page shows
// loading and failures itself.
const rootRoute = createRootRouteWithContext<{ queryClient: QueryClient }>()({ notFoundComponent: PageNotFound });

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
  errorComponent: WindowError,
});

// Every page needs a session in the web UI (TanStack Router authenticated
// routes). AppLayout renders the window around the page.
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
  errorComponent: WindowError,
});
// The sidebar lists the pages with a title, in this order.
const overviewRoute = createRoute({ getParentRoute: () => appRoute, path: "/", component: OverviewPage, staticData: { title: "Overview", icon: LayoutGrid } });
const agentsRoute = createRoute({ getParentRoute: () => appRoute, path: "/agents", component: AgentsPage, staticData: { title: "Agents", icon: Bot } });
const usageRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/usage",
  validateSearch: validateUsageSearch,
  search: { middlewares: [stripSearchParams(USAGE_SEARCH_DEFAULTS)] },
  loaderDeps: ({ search }) => search,
  loader: ({ context, deps }) => {
    void context.queryClient.prefetchQuery(usagePageQuery(usageFilters(deps)));
  },
  component: UsagePage,
  staticData: { title: "Usage", icon: ChartNoAxesColumn },
});
const settingsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/settings",
  loader: ({ context }) => {
    if (distributionCapabilities.cliRegistration) void context.queryClient.prefetchQuery(cliRegistrationQuery());
  },
  component: SettingsPage,
  staticData: { title: "Settings", icon: Settings },
});

export const router = createRouter({
  routeTree: rootRoute.addChildren([
    signInRoute,
    appRoute.addChildren([overviewRoute, agentsRoute, usageRoute, settingsRoute]),
  ]),
  context: { queryClient },
  // Pages load on intent; TanStack Query decides whether the data is fresh.
  defaultPreload: "intent",
  defaultPreloadStaleTime: 0,
  // Each page starts at its top; the window scrolls the page, not the document.
  scrollToTopSelectors: ["#page-content"],
  // The web UI has real URLs. The desktop window has no address bar or deep
  // links, so its location stays in memory and the webview keeps loading index.html.
  history: web ? createBrowserHistory() : createMemoryHistory(),
  defaultErrorComponent: PageError,
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
    /** Shown by the sign-in page. */
    notice?: string;
  }
  interface StaticDataRouteOption {
    /** A page of the window: its heading and sidebar entry. */
    title?: string;
    icon?: LucideIcon;
  }
}
