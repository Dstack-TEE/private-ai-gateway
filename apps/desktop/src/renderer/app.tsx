import React, { useCallback, useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Outlet, useMatches, useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { useAgents } from "./hooks/use-agents";
import { useAppState } from "./lib/use-app-state";
import { errorMessage, toastError } from "./lib/error-message";
import { useUpdates } from "./updates";
import { INITIAL_STATE, type AboutLink, type AppState, type NavigationTarget } from "../shared/contracts";
import { PageHeader, Sidebar } from "./components/navigation";
import { desktopApi, distributionCapabilities, web } from "./lib/environment";
import { unavailableState } from "./lib/protection";
import { ShellContext, type AppDialog, type Shell } from "./lib/shell";
import { ProfileEditorDialog, ProfilesDialog } from "./features/profiles";
import { PrivacyDialog } from "./features/privacy";
import { LocalApiDialog } from "./features/local-api";
import { WebUiDialog } from "./features/web-ui";
import { UsageProofDialog } from "./features/usage";
import { LocalApiExamplesDialog } from "./components/local-api-examples";
import { NotificationsDialog } from "./components/notifications";
import { useConfirm, useConfirmOpen } from "./components/confirm";
import { useDialog } from "./components/app-dialog";
import { AppearanceProvider } from "./components/appearance";
import { Toaster } from "./components/ui/sonner";
import { localEndpoint } from "./lib/format";

/** The signed-in window: the sidebar, the page header and the page. */
export function AppLayout(): React.JSX.Element {
  const client = useQueryClient();
  const navigate = useNavigate();
  useEffect(() => desktopApi.onSettingsReset(() => {
    void client.resetQueries();
    void navigate({ to: "/settings", replace: true });
    toast.success("Settings reset");
  }), [client, navigate]);
  return <AppearanceProvider api={desktopApi}>
    <Window />
    <Toaster />
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
  const [startingBackend, setStartingBackend] = useState(false);
  const confirm = useConfirm();
  const confirming = useConfirmOpen();
  const dialog = useDialog<AppDialog>();
  const { payload: shownDialog, key: dialogKey, show: openDialog } = dialog;
  const [copied, setCopied] = useState<string>();
  const copyTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copyTimer.current), []);
  const { data: clientKey = "" } = useQuery({ queryKey: ["client-key"], queryFn: () => desktopApi.getClientKey(), enabled: backendReady });
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  useEffect(() => desktopApi.onClientKeyChange((available) => {
    if (available) void client.invalidateQueries({ queryKey: ["client-key"] });
    else client.setQueryData(["client-key"], "");
  }), [client]);
  const [applying, setApplying] = useState(false);

  /** For dialogs that present the failure themselves. */
  const applyState = async (action: () => Promise<AppState | void>): Promise<void> => {
    const next = await action();
    if (next) setState(next);
  };

  const copyValue = useCallback(async (label: string, value: string) => {
    await desktopApi.copyText(value);
    setCopied(label);
    toast.success(`${label} copied`);
    window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => setCopied((current) => (current === label ? undefined : current)), 1_400);
  }, []);

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
  const openProfileSetup = useCallback(() => {
    if (state.protection.phase === "starting") return;
    openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: state.profiles.some((profile) => profile.id === state.activeProfileId) });
  }, [state, openDialog]);

  // The shell's actions read only the values in its dependency list, so the
  // context changes exactly when one of them does.
  const shell = useMemo<Shell>(() => {
    const starting = state.protection.phase === "starting";
    const runAction = async (title: string, action: () => Promise<AppState | void>) => {
      try {
        const next = await action();
        if (next) setState(next);
      } catch (error) {
        toastError(title, error);
      }
    };
    const openProfiles = () => {
      if (starting) return;
      openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: false });
    };
    const toggleProtection = () => {
      const { action } = state.protection;
      if (!action.enabled) return;
      if (action.operation === "setUpProfile") {
        openProfileSetup();
        return;
      }
      void runAction(action.operation === "stop" ? "Could not stop protection" : "Could not start protection", () =>
        action.operation === "stop" ? desktopApi.stop() : desktopApi.start(state.config));
    };
    const setRequireProductionOs = async (required: boolean) => {
      if (applying) return;
      setApplying(true);
      try {
        if (state.protection.action.operation === "stop" && !await confirm({
          title: required ? "Require production OS?" : "Allow development OS?",
          message: "Protection stops before the policy changes.",
          confirmLabel: "Stop and Change",
        })) return;
        setState(await desktopApi.setRequireProductionOs(required));
      } catch (error) { toastError("Could not change the OS policy", error); }
      finally { setApplying(false); }
    };
    const resetSettings = async () => {
      // A browser has no window of the app to resize.
      const resetItems = new Intl.ListFormat("en").format([
        "appearance", "notifications", "startup preferences", "development OS policy", "update channel",
        "Local API settings", "web UI settings", ...web ? [] : ["window size"],
      ]);
      let confirmed: boolean;
      try {
        confirmed = await confirm({
          title: "Reset settings?",
          message: `${agents.accessStatus === "authorized" ? "Stop protection, disconnect all agents and restore their configurations," : "Stop protection"} and reset ${resetItems}. Profiles, credentials, the Local API key, and usage history are kept. This does not change system notification permission${distributionCapabilities.cliRegistration ? " or remove the pap command" : ""}.`,
          confirmLabel: "Reset Settings",
          destructive: true,
        });
      } catch (error) {
        toastError("Could not reset settings", error);
        return;
      }
      if (!confirmed) return;
      setApplying(true);
      try {
        setState(await desktopApi.resetSettings());
      } catch (error) {
        toastError("Could not reset settings", error);
      } finally {
        setApplying(false);
      }
    };
    const startBackend = async () => {
      if (startingBackend) return;
      setStartingBackend(true);
      try { setState(await desktopApi.startBackendService()); }
      catch (error) { toastError("Could not start the background service", error); }
      finally { setStartingBackend(false); }
    };
    return {
      state,
      agents,
      updates,
      clientKey,
      clientKeyVisible,
      toggleClientKey: () => setClientKeyVisible((visible) => !visible),
      copied,
      copyValue,
      copy: (label, value) => void runAction(`Could not copy the ${label.toLowerCase()}`, () => copyValue(label, value)),
      applying,
      startingBackend: startingBackend || starting,
      startBackend: () => void startBackend(),
      toggleProtection,
      setRequireProductionOs: (required) => void setRequireProductionOs(required),
      resetSettings: () => void resetSettings(),
      openDialog,
      openProfiles,
      openAboutLink: (target: AboutLink) => void runAction("Could not open the link", () => desktopApi.openAboutLink(target)),
    };
  }, [state, agents, updates, clientKey, clientKeyVisible, copied, copyValue, applying, startingBackend, setState, openDialog, openProfileSetup, confirm]);

  const requestStopAllAndQuit = useEffectEvent(async () => {
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
      toastError("Could not stop all services", error);
    }
  });
  useEffect(() => desktopApi.onStopAllRequest(() => { void requestStopAllAndQuit(); }), []);

  // A page or dialog requested by the menu bar, the tray or the Settings
  // shortcut shows once an open dialog has closed, so its draft is not lost.
  // A confirmation is answered first, as an alert is: the request ends there.
  const deferredRequest = useRef<NavigationTarget | undefined>(undefined);
  const show = (target: NavigationTarget) => {
    if (target === "profiles") shell.openProfiles();
    else if (target === "profile-setup") openProfileSetup();
    else void navigate({ to: `/${target}` });
  };
  const showRequested = useEffectEvent((target: NavigationTarget) => {
    if (dialog.control.open) deferredRequest.current = target;
    else if (!confirming) show(target);
  });
  useEffect(() => desktopApi.onNavigate((target) => showRequested(target)), []);
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
    <main className="w-full h-full grid grid-cols-[var(--sidebar-width)_minmax(0,_1fr)] overflow-hidden max-[780px]:grid-cols-[154px_minmax(0,_1fr)] max-[620px]:grid-cols-[68px_minmax(0,_1fr)] max-[440px]:grid-cols-[56px_minmax(0,_1fr)]">
      <Sidebar navigationRef={navigation} />
      <section className="min-w-0 min-h-0 flex flex-col bg-background">
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
          state={state} clientKey={clientKey} clientKeyVisible={clientKeyVisible} copied={copied}
          onCopy={copyValue} onToggleKey={() => setClientKeyVisible((visible) => !visible)}
          onRotate={rotateClientKey}
          onSave={(config) => applyState(() => desktopApi.saveLocalApiConfig(config))}
          {...dialogControl}
        />}
        {shownDialog?.kind === "local-api-example" && <LocalApiExamplesDialog
          apiKey={clientKey} endpoint={state.proxyUrl ?? localEndpoint(state.localApi)} models={state.catalog?.models ?? []}
          onCopy={(value) => desktopApi.copyText(value)} {...dialogControl}
        />}
        {shownDialog?.kind === "notifications" && <NotificationsDialog api={desktopApi} {...dialogControl} />}
        {shownDialog?.kind === "web-ui" && <WebUiDialog
          state={state} copied={copied} onCopy={copyValue}
          onSave={(config) => applyState(() => desktopApi.saveWebUi(config)).then(() => undefined, errorMessage)}
          onSetPassword={(password) => applyState(() => desktopApi.setWebUiPassword(password)).then(() => undefined, errorMessage)}
          {...dialogControl}
        />}
        {shownDialog?.kind === "usage-proof" && <UsageProofDialog activity={shownDialog.activity} {...dialogControl} />}
      </React.Fragment>
    </main>
    </ShellContext.Provider>
  );
}
