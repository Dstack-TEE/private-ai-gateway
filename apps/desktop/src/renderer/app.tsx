import React, { useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Outlet, useMatches, useNavigate } from "@tanstack/react-router";
import { SERVICE_AGENT, useAgents } from "./hooks/use-agents";
import { useAppState } from "./lib/use-app-state";
import { useUpdates } from "./updates";
import { INITIAL_STATE, type AboutLink, type AppState, type NavigationTarget } from "../shared/contracts";
import { PageHeader, Sidebar } from "./components/navigation";
import { desktopApi, distributionCapabilities } from "./lib/environment";
import { unavailableState } from "./lib/protection";
import { OS_POLICY_CHANGE, ShellContext, type AppDialog, type Shell } from "./lib/shell";
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
import { localEndpoint } from "./lib/format";

/** The signed-in window: the sidebar, the page header and the page. */
export function AppLayout(): React.JSX.Element {
  const client = useQueryClient();
  const navigate = useNavigate();
  // The reset settings show on the Settings page; only screen readers are told.
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => desktopApi.onSettingsReset(() => {
    void client.resetQueries();
    void navigate({ to: "/settings", replace: true });
    setAnnouncement("Settings reset");
  }), [client, navigate]);
  useEffect(() => {
    if (!announcement) return;
    const timer = window.setTimeout(() => setAnnouncement(""), 1_500);
    return () => window.clearTimeout(timer);
  }, [announcement]);
  return <AppearanceProvider api={desktopApi}>
    <Window />
    <span className="sr-only" role="status">{announcement}</span>
  </AppearanceProvider>;
}

function Window(): React.JSX.Element {
  const client = useQueryClient();
  const navigate = useNavigate();
  const updates = useUpdates(desktopApi, distributionCapabilities.nativeUpdates || distributionCapabilities.channel === "web");
  const appState = useAppState(desktopApi);
  const state = useMemo(() => appState.error ? unavailableState(appState.error) : appState.data ?? INITIAL_STATE, [appState.error, appState.data]);
  const setState = appState.setState;
  const agents = useAgents(desktopApi, state, distributionCapabilities.sandboxHomeAccess);
  const backendReady = Boolean(appState.data) && state.backendConnected !== false;
  const confirm = useConfirm();
  const reportFailure = useReportFailure();
  const confirming = useConfirmOpen();
  const dialog = useDialog<AppDialog>();
  const { payload: shownDialog, key: dialogKey, show: openDialog } = dialog;
  const { data: clientKey = "" } = useQuery({ queryKey: ["client-key"], queryFn: () => desktopApi.getClientKey(), enabled: backendReady });
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  useEffect(() => desktopApi.onClientKeyChange((available) => {
    if (available) void client.invalidateQueries({ queryKey: ["client-key"] });
    else client.setQueryData(["client-key"], "");
  }), [client]);
  const changeRequireProductionOs = async (required: boolean) => {
    if (state.protection.action.operation === "stop" && !await confirm({
      title: required ? "Require production OS?" : "Allow development OS?",
      message: "Protection stops before the policy changes.",
      confirmLabel: "Stop and Change",
    })) return;
    setState(await desktopApi.setRequireProductionOs(required));
  };
  const changingOsPolicy = useIsMutating({ mutationKey: OS_POLICY_CHANGE }) > 0;
  const reset = useMutation({
    mutationFn: () => desktopApi.resetSettings(),
    onSuccess: setState,
    onError: (error) => reportFailure("Could not reset settings", error),
  });
  const protection = useMutation({
    mutationFn: (operation: "start" | "stop") => operation === "stop" ? desktopApi.stop() : desktopApi.start(state.config),
    onSuccess: setState,
    onError: (error, operation) => reportFailure(operation === "stop" ? "Could not stop protection" : "Could not start protection", error),
  });
  const backendStart = useMutation({
    mutationFn: () => desktopApi.startBackendService(),
    onSuccess: setState,
    onError: (error) => reportFailure("Could not start the background service", error),
  });
  /** A settings change is applying; controls that change settings wait. */
  const applying = changingOsPolicy || reset.isPending;
  const { mutate: resetMutate } = reset;
  const { mutate: startBackend, isPending: startingBackend } = backendStart;
  const { mutate: toggleProtection, isPending: protectionPending } = protection;

  /** For dialogs that present the failure themselves. */
  const applyState = async (action: () => Promise<AppState | void>): Promise<void> => {
    const next = await action();
    if (next) setState(next);
  };

  const rotateClientKey = async (): Promise<void> => {
    try {
      client.setQueryData(["client-key"], await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
    } catch (error) {
      setClientKeyVisible(false);
      throw error;
    }
  };

  // Protection needs a usable profile first: create one, or fix the active one.
  const starting = state.protection.phase === "starting";
  const openProfileSetup = () => {
    if (starting) return;
    openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: state.profiles.some((profile) => profile.id === state.activeProfileId) });
  };

  const runAction = async (title: string, action: () => Promise<AppState | void>) => {
    try {
      const next = await action();
      if (next) setState(next);
    } catch (error) {
      reportFailure(title, error);
    }
  };
  const resetSettings = async () => {
    // What a reset leaves alone; the web UI has no notifications or pap command.
    const kept = new Intl.ListFormat("en").format([
      "Profiles", "credentials", "the Local API key", "usage history",
      ...distributionCapabilities.notifications ? ["system notification permission"] : [],
      ...distributionCapabilities.cliRegistration ? ["the pap command"] : [],
    ]);
    try {
      if (await confirm({
        title: "Reset settings?",
        message: `${agents.accessStatus === "authorized" ? "Protection stops, agents disconnect and their configurations are restored," : "Protection stops"} and settings return to their defaults. ${kept} are unchanged.`,
        confirmLabel: "Reset Settings",
        destructive: true,
      })) resetMutate();
    } catch (error) {
      reportFailure("Could not reset settings", error);
    }
  };
  // The state changes with every backend update, so the shell is not memoized.
  const shell: Shell = {
    state,
    agents,
    updates,
    clientKey,
    clientKeyVisible,
    toggleClientKey: () => setClientKeyVisible((visible) => !visible),
    applying,
    startingBackend: startingBackend || starting,
    startBackend: () => {
      if (!startingBackend) startBackend();
    },
    protectionPending,
    toggleProtection: () => {
      const { action } = state.protection;
      if (!action.enabled || protectionPending) return;
      if (action.operation === "setUpProfile") openProfileSetup();
      else toggleProtection(action.operation);
    },
    changeRequireProductionOs,
    resetSettings: () => void resetSettings(),
    openDialog,
    openProfiles: () => {
      if (starting) return;
      openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: false });
    },
    openAboutLink: (target: AboutLink) => void runAction("Could not open the link", () => desktopApi.openAboutLink(target)),
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
    if (target === "profiles") shell.openProfiles();
    else if (target === "profile-setup") openProfileSetup();
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
  // agent access.
  const ready = Boolean(appState.data || appState.error) && (agents.accessStatus !== undefined || agents.problem !== undefined);
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
        <div id="page-content" className="flex-auto min-w-0 min-h-0 overflow-auto pt-4 pr-6 pb-6 pl-6 [&_>_[role=alert]]:mb-4 max-[780px]:p-4 max-[440px]:p-3">
          <Outlet />
        </div>
      </section>

      <React.Fragment key={dialogKey}>
        {shownDialog?.kind === "profiles" && <ProfilesDialog
          state={state} repair={shownDialog.repair}
          onActivate={(profileId) => applyState(() => desktopApi.activateProfile(profileId))}
          onSave={(profile, key) => applyState(() => desktopApi.saveConfiguration(profile, state.config.requireProductionOs, key))}
          onDelete={(profileId) => applyState(() => desktopApi.deleteProfile(profileId))}
          {...dialogControl}
        />}
        {shownDialog?.kind === "setup-profile" && <ProfileEditorDialog
          state={state} startAfterSave
          onSave={(profile, key) => applyState(async () => {
            const saved = await desktopApi.saveConfiguration(profile, state.config.requireProductionOs, key);
            return desktopApi.start(saved.config);
          })}
          onDelete={(profileId) => applyState(() => desktopApi.deleteProfile(profileId))}
          onComplete={dialogControl.onClose} {...dialogControl}
        />}
        {shownDialog?.kind === "privacy" && <PrivacyDialog state={state} {...dialogControl} />}
        {shownDialog?.kind === "local-api" && <LocalApiDialog
          state={state} clientKey={clientKey} clientKeyVisible={clientKeyVisible}
          onToggleKey={() => setClientKeyVisible((visible) => !visible)}
          onRotate={rotateClientKey}
          onSave={(config) => applyState(() => desktopApi.saveLocalApiConfig(config))}
          {...dialogControl}
        />}
        {shownDialog?.kind === "local-api-example" && <LocalApiExamplesDialog
          apiKey={clientKey} endpoint={state.proxyUrl ?? localEndpoint(state.localApi)} models={state.catalog?.models ?? []}
          {...dialogControl}
        />}
        {shownDialog?.kind === "notifications" && <NotificationsDialog api={desktopApi} {...dialogControl} />}
        {shownDialog?.kind === "web-ui" && <WebUiDialog
          state={state}
          onSave={(config) => applyState(() => desktopApi.saveWebUi(config))}
          onSetPassword={(password) => applyState(() => desktopApi.setWebUiPassword(password))}
          {...dialogControl}
        />}
        {shownDialog?.kind === "usage-proof" && <UsageProofDialog activity={shownDialog.activity} {...dialogControl} />}
      </React.Fragment>
    </main>
    </ShellContext.Provider>
  );
}
