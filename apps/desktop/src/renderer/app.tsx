import React, { useEffect, useEffectEvent, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Outlet, useMatches, useNavigate } from "@tanstack/react-router";
import { SERVICE_AGENT, useAgents } from "./hooks/use-agents";
import { useAppState } from "./lib/use-app-state";
import { useUpdates } from "./updates";
import { INITIAL_STATE, type NavigationTarget } from "../shared/contracts";
import { PageHeader, Sidebar } from "./components/navigation";
import { desktopApi, distributionCapabilities } from "./lib/environment";
import { activeProfile, unavailableState } from "./lib/protection";
import { ShellContext, type AppDialog, type Shell } from "./lib/shell";
import { ProfileEditorDialog, ProfilesDialog } from "./features/profiles";
import { PrivacyDialog } from "./features/privacy";
import { LocalApiDialog } from "./features/local-api";
import { WebUiDialog } from "./features/web-ui";
import { UsageProofDialog } from "./features/usage";
import { LocalApiExamplesDialog } from "./components/local-api-examples";
import { NotificationsDialog } from "./components/notifications";
import { useConfirm, useConfirmOpen, useReportFailure } from "./components/confirm";
import { useDialog } from "./components/app-dialog";
import { AppearanceProvider } from "./components/appearance";
import { AuthoredError } from "./lib/error-message";
import { cliRegistrationQuery, clientKeyQuery } from "./lib/queries";

/** The signed-in window: the sidebar, the page header and the page. */
export function AppLayout(): React.JSX.Element {
  const client = useQueryClient();
  const navigate = useNavigate();
  // The reset settings show on the Settings page; only screen readers are told.
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => desktopApi.onSettingsReset(() => {
    // The window's state is the backend's, which a reset doesn't clear.
    void client.resetQueries({ predicate: (query) => query.queryKey[0] !== "app-state" });
    void navigate({ to: "/settings", replace: true });
    setAnnouncement("Settings reset");
  }), [client, navigate]);
  useEffect(() => {
    if (!announcement) return;
    const timer = window.setTimeout(() => setAnnouncement(""), 1_500);
    return () => window.clearTimeout(timer);
  }, [announcement]);
  return <AppearanceProvider>
    <Window />
    <span className="sr-only" role="status">{announcement}</span>
  </AppearanceProvider>;
}

function Window(): React.JSX.Element {
  const client = useQueryClient();
  const navigate = useNavigate();
  const updates = useUpdates();
  const appState = useAppState(desktopApi);
  const state = appState.error ? unavailableState(appState.error) : appState.data ?? INITIAL_STATE;
  const setState = appState.setState;
  const backendReady = Boolean(appState.data) && state.backendConnected !== false;
  const agents = useAgents(state, backendReady);
  const confirm = useConfirm();
  const reportFailure = useReportFailure();
  const confirming = useConfirmOpen();
  const dialog = useDialog<AppDialog>();
  const { payload: shownDialog, key: dialogKey, show: openDialog } = dialog;
  const { data: clientKey } = useQuery({ ...clientKeyQuery, enabled: backendReady });
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  useEffect(() => desktopApi.onClientKeyChange((available) => {
    if (available) void client.invalidateQueries({ queryKey: clientKeyQuery.queryKey });
    else client.setQueryData(clientKeyQuery.queryKey, "");
  }), [client]);
  // The shell registers the `pap` command at startup; its failure is reported once.
  const { data: cliRegistration } = useQuery({ ...cliRegistrationQuery, enabled: distributionCapabilities.cliRegistration, staleTime: Infinity });
  const cliStartupError = cliRegistration?.startupError;
  const reportedCliStartupError = useRef<string>(undefined);
  useEffect(() => {
    if (!cliStartupError || cliStartupError === reportedCliStartupError.current) return;
    reportedCliStartupError.current = cliStartupError;
    reportFailure("Could not register the pap command", new AuthoredError(cliStartupError));
  }, [cliStartupError, reportFailure]);

  const osPolicy = useMutation({
    mutationFn: async (required: boolean) => {
      if (state.protection.action.operation === "stop" && !await confirm({
        title: required ? "Require production OS?" : "Allow development OS?",
        message: "Protection stops before the policy changes.",
        confirmLabel: "Stop and Change",
      })) return;
      setState(await desktopApi.setRequireProductionOs(required));
    },
    meta: { errorTitle: "Could not change the OS policy" },
  });
  const reset = useMutation({
    mutationFn: () => desktopApi.resetSettings(),
    onSuccess: setState,
    meta: { errorTitle: "Could not reset settings" },
  });
  const protection = useMutation({
    mutationFn: (operation: "start" | "stop") => operation === "stop" ? desktopApi.stop() : desktopApi.start(state.config),
    onSuccess: setState,
    onError: (error, operation) => reportFailure(operation === "stop" ? "Could not stop protection" : "Could not start protection", error),
  });
  const backendStart = useMutation({
    mutationFn: () => desktopApi.startBackendService(),
    onSuccess: setState,
    meta: { errorTitle: "Could not start the background service" },
  });
  /** A settings change is applying; controls that change settings wait. */
  const applying = osPolicy.isPending || reset.isPending;

  // Protection needs a usable profile first: create one, or `repair` the active one.
  const starting = state.protection.phase === "starting";
  const openProfiles = (repair: boolean) => {
    if (starting) return;
    openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: repair && Boolean(activeProfile(state)) });
  };

  const resetSettings = async () => {
    // What a reset leaves alone; the web UI has no notifications or pap command.
    const kept = new Intl.ListFormat("en").format([
      "Profiles", "credentials", "the Local API key", "usage history",
      ...distributionCapabilities.notifications ? ["system notification permission"] : [],
      ...distributionCapabilities.cliRegistration ? ["the pap command"] : [],
    ]);
    if (await confirm({
      title: "Reset settings?",
      message: `${agents.accessStatus === "authorized" ? "Protection stops, agents disconnect and their configurations are restored," : "Protection stops"} and settings return to their defaults. ${kept} are unchanged.`,
      confirmLabel: "Reset Settings",
      destructive: true,
    })) reset.mutate();
  };
  // The state changes with every backend update, so the shell is not memoized.
  const shell: Shell = {
    state,
    agents,
    updates,
    clientKey,
    clientKeyVisible,
    toggleClientKey: () => setClientKeyVisible((visible) => !visible),
    rotateClientKey: async () => {
      try {
        client.setQueryData(clientKeyQuery.queryKey, await desktopApi.rotateClientKey());
        setClientKeyVisible(true);
      } catch (error) {
        setClientKeyVisible(false);
        throw error;
      }
    },
    applying,
    startingBackend: backendStart.isPending || starting,
    startBackend: () => {
      if (!backendStart.isPending) backendStart.mutate();
    },
    protectionPending: protection.isPending,
    toggleProtection: () => {
      const { action } = state.protection;
      if (!action.enabled || protection.isPending) return;
      if (action.operation === "setUpProfile") openProfiles(true);
      else protection.mutate(action.operation);
    },
    changeRequireProductionOs: (required) => {
      if (!applying) osPolicy.mutate(required);
    },
    resetSettings: () => void resetSettings(),
    openDialog,
    openProfiles: () => openProfiles(false),
    openAboutLink: (target) => void desktopApi.openAboutLink(target).catch((error: unknown) => reportFailure("Could not open the link", error)),
  };

  // A repeated request while one asks is ignored: one answer settles it.
  const askingStopAll = useRef(false);
  const requestStopAllAndQuit = useEffectEvent(async () => {
    if (askingStopAll.current) return;
    askingStopAll.current = true;
    try {
      const confirmed = await confirm({
        title: "Stop all services and quit?",
        message: agents.accessStatus === "authorized"
          ? "This stops protection, restores managed agent configurations, shuts down the background service, and quits the app. In-flight requests may be interrupted."
          : "This stops protection, shuts down the background service, and quits the app. In-flight requests may be interrupted.",
        confirmLabel: "Stop All and Quit",
      });
      if (confirmed) await desktopApi.stopAllAndQuit();
    } catch (error) {
      reportFailure("Could not stop all services", error);
    } finally {
      askingStopAll.current = false;
    }
  });

  // A page or dialog requested by the menu bar, the tray or the Settings
  // shortcut shows once an open dialog has closed, so its draft is not lost.
  // A confirmation is answered first, as an alert is: the request ends there.
  // Stopping everything, or Codex's service, asks at once, over any dialog.
  type PageRequest = Exclude<NavigationTarget, "confirm-stop-all" | "confirm-codex-service-stop">;
  const deferredRequest = useRef<PageRequest | undefined>(undefined);
  const show = (target: PageRequest) => {
    if (target === "profiles" || target === "profile-setup") openProfiles(target === "profile-setup");
    else void navigate({ to: `/${target}` });
  };
  const showRequested = useEffectEvent((target: NavigationTarget) => {
    if (target === "confirm-stop-all") void requestStopAllAndQuit();
    else if (target === "confirm-codex-service-stop") void agents.offerServiceStop(SERVICE_AGENT);
    else if (dialog.control.open) deferredRequest.current = target;
    else if (!confirming) show(target);
  });
  // A request waits in the shell until the window's state and agents have
  // loaded, so one made while the app starts sees the real profiles and
  // agent access, or until they can't be read, as when the backend could not start.
  const ready = Boolean(appState.data || appState.error) && (agents.accessStatus !== undefined || (agents.problem && state.protection.phase !== "starting"));
  useEffect(() => ready ? desktopApi.onNavigate((target) => showRequested(target)) : undefined, [ready]);
  const dialogControl = {
    ...dialog.control,
    onOpenChangeComplete: (open: boolean) => {
      dialog.control.onOpenChangeComplete(open);
      const target = deferredRequest.current;
      if (open || !target) return;
      deferredRequest.current = undefined;
      show(target);
    },
  };

  // Navigating from the sidebar keeps focus there; any other navigation,
  // including history traversal, lands on the new page heading.
  const page = useMatches({ select: (matches) => matches.at(-1)?.routeId });
  const shownPage = useRef(page);
  const navigation = useRef<HTMLElement>(null);
  const pageTitle = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    if (shownPage.current === page) return;
    shownPage.current = page;
    if (!navigation.current?.contains(document.activeElement)) pageTitle.current?.focus();
  }, [page]);

  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.key !== "," || !(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      event.preventDefault();
      showRequested("settings");
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, []);

  return (
    <ShellContext.Provider value={shell}>
    <main className="w-full h-full grid grid-cols-[var(--sidebar-width)_minmax(0,_1fr)] overflow-hidden bg-background max-[780px]:grid-cols-[154px_minmax(0,_1fr)] max-[620px]:grid-cols-[68px_minmax(0,_1fr)] max-[440px]:grid-cols-[56px_minmax(0,_1fr)]">
      <Sidebar navigationRef={navigation} />
      <section className="min-w-0 min-h-0 flex flex-col">
        <PageHeader titleRef={pageTitle} />
        <div id="page-content" className="min-h-0 min-w-0 flex-auto overflow-auto px-6 pt-4 pb-6 max-[780px]:p-4 max-[440px]:p-3">
          <Outlet />
        </div>
      </section>

      <React.Fragment key={dialogKey}>
        {shownDialog?.kind === "profiles" && <ProfilesDialog repair={shownDialog.repair} {...dialogControl} />}
        {shownDialog?.kind === "setup-profile" && <ProfileEditorDialog startAfterSave {...dialogControl} />}
        {shownDialog?.kind === "privacy" && <PrivacyDialog {...dialogControl} />}
        {shownDialog?.kind === "local-api" && <LocalApiDialog {...dialogControl} />}
        {shownDialog?.kind === "local-api-example" && <LocalApiExamplesDialog {...dialogControl} />}
        {shownDialog?.kind === "notifications" && <NotificationsDialog {...dialogControl} />}
        {shownDialog?.kind === "web-ui" && <WebUiDialog {...dialogControl} />}
        {shownDialog?.kind === "usage-proof" && <UsageProofDialog activity={shownDialog.activity} {...dialogControl} />}
      </React.Fragment>
    </main>
    </ShellContext.Provider>
  );
}
