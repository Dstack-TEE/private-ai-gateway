import { useAgents } from "./hooks/use-agents";
import React, { useCallback, useEffect, useEffectEvent, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { cliRegistrationQuery, usagePageQuery } from "./lib/page-queries";
import { useAppState } from "./lib/use-app-state";
import { useWindowReady } from "./lib/use-window-ready";
import { errorMessage, toastError } from "./lib/error-message";
import { brand } from "./brand/brand";
import { useUpdates } from "./updates";
import type { AgentStatus, ConfidentialProfile, AppState, LaunchPreferences, NavigationTarget, RequestActivity } from "../shared/contracts";
import { PageHeader, Sidebar, useView } from "./components/navigation";
import type { SettingsTarget } from "./components/navigation";
import { desktopApi, distributionCapabilities } from "./lib/environment";
import { INITIAL_STATE, protectionFlags, profileIsAvailable, unavailableState } from "./lib/protection";
import { AgentsView } from "./features/agents";
import { Overview } from "./features/overview";
import { UsageView } from "./features/usage";
import { SettingsView } from "./features/settings";
import { ProfileEditorDialog, ProfilesDialog } from "./features/profiles";
import { PrivacyDialog } from "./features/privacy";
import { LocalApiDialog } from "./features/local-api";
import { WebUiDialog } from "./features/web-ui";
import { UsageProofDialog } from "./features/usage";
import { LocalApiExamplesDialog } from "./components/local-api-examples";
import { NotificationsDialog } from "./components/notifications";
import { useConfirm } from "./components/confirm";
import { localEndpoint } from "./lib/format";

type AppDialog =
  | { kind: "profiles"; repair: boolean }
  | { kind: "setup-profile" | "privacy" | "local-api" | "local-api-example" | "notifications" | "web-ui" }
  | { kind: "usage-proof"; activity: RequestActivity };

/**
 * Whether a modal dialog, confirmation or sheet is open. Requests to show a
 * page, from the Settings shortcut, the macOS menu accelerator or the tray,
 * wait until it closes; popovers such as the date picker do not block them.
 */
function modalOpen(): boolean {
  return Boolean(document.querySelector("[data-slot=dialog-content], [data-slot=alert-dialog-content], [data-slot=sheet-content]"));
}

export function App(): React.JSX.Element {
  const updates = useUpdates(desktopApi, distributionCapabilities.nativeUpdates || distributionCapabilities.channel === "web");
  const view = useView();
  const navigate = useNavigate();
  const routeNotice = useLocation({ select: (location) => location.state.notice });
  const appState = useAppState(desktopApi, INITIAL_STATE);
  const state = appState.error ? unavailableState(appState.error) : appState.data ?? INITIAL_STATE;
  const setState = appState.setState;
  const stateLoaded = !appState.isLoading;
  const [allowDevelopmentOs, setAllowDevelopmentOs] = useState(false);
  const client = useQueryClient();
  const backendReady = Boolean(appState.data) && state.backendConnected !== false;
  useEffect(() => {
    if (!backendReady) return;
    // Prefetch shares page caches; failures are presented when the page is opened.
    void client.prefetchQuery(usagePageQuery());
    if (distributionCapabilities.cliRegistration) void client.prefetchQuery(cliRegistrationQuery());
  }, [client, backendReady]);
  const { data: launchPreferences } = useQuery({ queryKey: ["launch-preferences"], queryFn: () => desktopApi.getLaunchPreferences() });
  const [savingPreference, setSavingPreference] = useState(false);
  const [connectingBackend, setConnectingBackend] = useState(false);
  const reportWindowError = useCallback((message: string) => toastError("Window unavailable", message), []);
  useWindowReady(stateLoaded, desktopApi.mainWindowReady, reportWindowError);
  const confirm = useConfirm();
  const [dialog, setDialog] = useState<AppDialog>();
  // A tray or menu request never replaces a dialog that is already open.
  const openDialog = useCallback((next: AppDialog) => setDialog((current) => current ?? next), []);
  const closeDialog = useCallback(() => setDialog(undefined), []);
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [applying, setApplying] = useState(false);
  const [notice, setNotice] = useState<{ id: number; text: string } | undefined>();
  const notify = useCallback((message: string) => setNotice({ id: Date.now(), text: message }), []);
  const previousProfiles = useRef<ConfidentialProfile[] | undefined>(undefined);
  useEffect(() => {
    if (!stateLoaded) return;
    const previous = previousProfiles.current;
    previousProfiles.current = state.profiles;
    if (!previous) return;
    const saved = state.profiles.find((profile) => profile.auth.kind === "oauth" && profile.credentialSaved &&
      !previous.some((old) => old.id === profile.id && old.credentialRef === profile.credentialRef && old.verifiedAt === profile.verifiedAt));
    if (saved) notify(`${saved.name} saved`);
  }, [state.profiles, stateLoaded]);

  const copyTimer = useRef<number | undefined>(undefined);
  const { busy, running, verified, endpointDown } = protectionFlags(state);
  const { agents, accessStatus: agentAccessStatus, authorizing: authorizingAgents, controlsLocked: agentControlsLocked, pendingAgentChanges, loadAgents, requestAccess: requestAgentAccess, applyAgent, problem: agentProblem } = useAgents(desktopApi, {
    requiresAuthorization: distributionCapabilities.sandboxHomeAccess,
    active: view === "agents", revision: state.catalog?.revision, verified, notify,
  });
  useEffect(() => desktopApi.onLaunchPreferencesChange((next) => {
    void client.cancelQueries({ queryKey: ["launch-preferences"] }).then(() => client.setQueryData(["launch-preferences"], next));
  }), [client]);

  const saveLaunchPreference = async (name: keyof LaunchPreferences, enabled: boolean) => {
    setSavingPreference(true);
    try { await client.cancelQueries({ queryKey: ["launch-preferences"] }); client.setQueryData(["launch-preferences"], await desktopApi.setLaunchPreference(name, enabled)); }
    catch (error) { toastError("Could not change the launch preference", error); }
    finally { setSavingPreference(false); }
  };

  const requestStopAllAndQuit = useCallback(async () => {
    try {
      const confirmed = await confirm({
        title: "Stop all services and quit?",
        message: agentAccessStatus === "authorized"
          ? "This stops protection, restores managed agent configurations, shuts down the background service, and quits the app. In-flight requests may be interrupted."
          : "This stops protection, shuts down the background service, and quits the app. In-flight requests may be interrupted.",
        confirmLabel: "Stop All and Quit",
      });
      if (confirmed) await desktopApi.stopAllAndQuit();
    } catch (error) {
      toastError("Could not stop all services", error);
    }
  }, [agentAccessStatus, confirm]);

  useEffect(() => desktopApi.onStopAllRequest(() => { void requestStopAllAndQuit(); }), [requestStopAllAndQuit]);

  useEffect(() => {
    document.title = brand.productName;
  }, []);

  // Protection needs a usable profile first: create one, or fix the active one.
  const openProfileSetup = () => {
    openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: state.profiles.some((profile) => profile.id === state.activeProfileId) });
  };
  const showRequested = useEffectEvent((target: NavigationTarget) => {
    if (target === "profiles") openDialog({ kind: "profiles", repair: false });
    else if (target === "profile-setup") openProfileSetup();
    else if (target === "documentation" || target === "github") openAboutLink(target);
    else if (!modalOpen()) void navigate({ to: `/${target}` as const });
  });
  useEffect(() => desktopApi.onNavigate((target) => showRequested(target)), []);

  // Moving through the sidebar with arrow keys keeps focus there; any other
  // navigation, including history traversal, lands on the new page heading.
  const shownView = useRef(view);
  useEffect(() => {
    if (shownView.current === view) return;
    shownView.current = view;
    if (!document.querySelector("#main-navigation :focus-visible")) document.getElementById(`page-title-${view}`)?.focus();
  }, [view]);

  useEffect(() => {
    let active = true;
    let keyRead = 0;
    const loadClientKey = () => {
      const read = ++keyRead;
      void desktopApi.getClientKey().then(
        (key) => {
          if (!active || read !== keyRead) return;
          setClientKey(key);
        },
        () => {
          if (!active || read !== keyRead) return;
          setClientKey("");
        },
      );
    };
    loadClientKey();
    const unsubscribeClientKey = desktopApi.onClientKeyChange((available) => {
      if (!active) return;
      if (!available) {
        keyRead += 1;
        setClientKey("");
        setClientKeyVisible(false);
        return;
      }
      loadClientKey();
    });
    return () => {
      active = false;
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      unsubscribeClientKey();
    };
  }, []);

  // The form mirrors the configuration the backend will start with, so a
  // start from the tray switch shows up here too.
  const configuredPolicy = state.config.requireProductionOs;
  useEffect(() => {
    setAllowDevelopmentOs(!configuredPolicy);
  }, [configuredPolicy]);



  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.key !== "," || !(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      if (modalOpen()) return;
      event.preventDefault();
      void navigate({ to: "/settings" });
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [navigate]);


  const applyStateAction = async (action: () => Promise<AppState | void>, notice?: string): Promise<string | undefined> => {
    try {
      const next = await action();
      if (next) {
        setState(next);
      }
      if (notice) notify(notice);
    } catch (error) {
      return errorMessage(error);
    }
  };

  const runAction = async (title: string, action: () => Promise<AppState | void>, notice?: string) => {
    const message = await applyStateAction(action, notice);
    if (message) toastError(title, message);
  };

  const toggleProtection = () => {
    const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
    if (!running && !busy && !state.reconnecting && !profileIsAvailable(activeProfile, state)) {
      openProfileSetup();
      return;
    }
    void runAction(running || busy || state.reconnecting ? "Could not stop protection" : "Could not start protection", () =>
      running || busy || state.reconnecting ? desktopApi.stop() : desktopApi.start({ remoteUrl: state.config.remoteUrl, requireProductionOs: !allowDevelopmentOs }),
    );
  };

  const changeDevelopmentOs = async (enabled: boolean) => {
    if (applying) return;
    setApplying(true);
    try {
      if (!await confirm({ title: enabled ? "Allow development OS?" : "Require production OS?", message: "Protection will stop before changing this policy.", confirmLabel: "Stop and Change" })) return;
      setState(await desktopApi.stop());
      setAllowDevelopmentOs(enabled);
    } catch (error) { toastError("Could not change the OS policy", error); }
    finally { setApplying(false); }
  };

  const rotateClientKey = async (): Promise<string | undefined> => {
    try {
      setClientKey(await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
      return undefined;
    } catch (error) {
      setClientKey("");
      setClientKeyVisible(false);
      return errorMessage(error);
    }
  };

  /** Copies a value and marks it copied; the caller presents a failure. */
  const copyValue = async (label: string, value: string) => {
    await desktopApi.copyText(value);
    setCopied(label);
    notify(`${label} copied`);
    if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(
      () => setCopied((current) => (current === label ? undefined : current)),
      1_400,
    );
  };
  const copy = (label: string, value: string) => runAction(`Could not copy ${label.toLowerCase()}`, () => copyValue(label, value));
  const openAboutLink = (target: "documentation" | "github") => void runAction("Could not open the link", () => desktopApi.openAboutLink(target));

  const resetSettings = async () => {
    let confirmed: boolean;
    try {
      confirmed = await confirm({
        title: "Reset settings?",
        message: agentAccessStatus === "authorized"
          ? "Stop protection, disconnect all agents and restore their configurations, and reset appearance, notifications, startup preferences, development OS policy, update channel, Local API settings, web UI, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission or uninstall the private-ai-proxy command."
          : "Stop protection and reset appearance, notifications, startup preferences, development OS policy, Local API settings, web UI, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission.",
        confirmLabel: "Reset settings",
      });
    } catch (error) {
      toastError("Could not reset settings", error);
      return;
    }
    if (!confirmed) return;
    setApplying(true);
    try {
      setState(await desktopApi.resetSettings());
      await loadAgents();
      notify("Settings reset");
      window.setTimeout(() => document.getElementById("page-title-settings")?.focus(), 0);
    } catch (error) {
      toastError("Could not reset settings", error);
    } finally {
      setApplying(false);
    }
  };

  const locked = applying;
  const openSettings = (target: SettingsTarget) => {
    if (target !== "confidential") openDialog({ kind: target });
    else openDialog(state.profiles.length === 0 ? { kind: "setup-profile" } : { kind: "profiles", repair: false });
  };
  const inspectUsage = useCallback((activity: RequestActivity) => openDialog({ kind: "usage-proof", activity }), [openDialog]);

  const selectAgent = (agent: AgentStatus, connect: boolean) => {
    void applyAgent(agent, connect);
  };

  const startBackend = async () => {
    if (connectingBackend) return;
    setConnectingBackend(true);
    try { setState(await desktopApi.startBackendService()); }
    catch (error) { toastError("Could not start the background service", error); }
    finally { setConnectingBackend(false); }
  };

  const windowContent = (
    <main className="app-shell w-full h-full grid grid-cols-[var(--sidebar-width)_minmax(0,_1fr)] overflow-hidden bg-background max-[780px]:grid-cols-[154px_minmax(0,_1fr)] max-[620px]:grid-cols-[68px_minmax(0,_1fr)] max-[440px]:grid-cols-[56px_minmax(0,_1fr)]">
      <Sidebar updateReady={updates.ready} updateBusy={Boolean(updates.busy)} onRestartUpdate={() => void updates.restart()} />
      <section className="workspace min-w-0 min-h-0 flex flex-col">
        <PageHeader
          view={view}
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
          developmentMode={allowDevelopmentOs}
          onToggle={toggleProtection}
        />
        <div className="content flex-auto min-w-0 min-h-0 overflow-auto pt-4 pr-6 pb-6 pl-6 [&_>_[role=alert]]:mb-4 max-[780px]:p-4 max-[440px]:p-3" id={`page-${view}`} key={view}>
        {view === "overview" && (
          <Overview
            pendingAgentChanges={pendingAgentChanges}
            state={state}
            agents={agents}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            developmentMode={allowDevelopmentOs}
            backendDisconnected={state.backendConnected === false}
            connectingBackend={connectingBackend}
            agentProblem={agentProblem}
            accountApi={desktopApi}
            agentAccessStatus={agentAccessStatus}
            authorizingAgents={authorizingAgents}
            onAuthorizeAgents={() => void requestAgentAccess()}
            locked={locked || agentControlsLocked}
            clientKey={clientKey}
            clientKeyVisible={clientKeyVisible}
            copied={copied}
            onToggle={toggleProtection}
            onStartBackend={() => void startBackend()}
            onSettings={() => openSettings("confidential")}
            onPrivacy={() => openSettings("privacy")}
            onLocalSettings={() => openSettings("local-api")}
            onLocalExamples={() => openSettings("local-api-example")}
            onCopy={copy}
            onToggleClientKey={() => setClientKeyVisible((visible) => !visible)}
            onSelect={selectAgent}
            onInspect={inspectUsage}
          />
        )}
        {view === "agents" && (
          <AgentsView
            accessStatus={agentAccessStatus}
            authorizing={authorizingAgents}
            pendingAgentChanges={pendingAgentChanges}
            agents={agents}
            locked={locked || agentControlsLocked}
            problem={agentProblem}
            onSelect={selectAgent}
            onAuthorize={() => void requestAgentAccess()}
            onRetry={() => void requestAgentAccess()}
          />
        )}
        {view === "usage" && (
          <UsageView
            state={state}
            agents={agents}
            onInspect={inspectUsage}
          />
        )}
        {view === "settings" && (
          <SettingsView
            updates={updates}
            distribution={distributionCapabilities}
            state={state}
            busy={busy}
            running={running}
            allowDevelopmentOs={allowDevelopmentOs}
            locked={locked || Object.keys(pendingAgentChanges).length > 0}
            onPolicy={(value) => void changeDevelopmentOs(value)}
            onResetSettings={() => void resetSettings()}
            onAboutLink={openAboutLink}
            onOpen={openSettings}
            launchPreferences={launchPreferences}
            savingPreference={savingPreference}
            onLaunchPreference={(name, enabled) => void saveLaunchPreference(name, enabled)}
          />
        )}
        </div>
      </section>

      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {notice?.text ?? routeNotice}
      </div>
      {dialog?.kind === "profiles" && <ProfilesDialog
        state={state} busy={busy} running={running} repair={dialog.repair}
        onActivate={(profileId) => applyStateAction(() => desktopApi.activateProfile(profileId))}
        onSave={(profile, key) => applyStateAction(() => desktopApi.saveConfiguration(profile, state.config.requireProductionOs, key))}
        onDelete={(profileId) => applyStateAction(() => desktopApi.deleteProfile(profileId))}
        onClose={closeDialog}
      />}
      {dialog?.kind === "setup-profile" && <ProfileEditorDialog
        state={state} busy={busy} running={running} startAfterSave
        onSave={(profile, key) => applyStateAction(async () => {
          const saved = await desktopApi.saveConfiguration(profile, state.config.requireProductionOs, key);
          return desktopApi.start(saved.config);
        })}
        onDelete={(profileId) => applyStateAction(() => desktopApi.deleteProfile(profileId))}
        onComplete={closeDialog} onDeleted={closeDialog} onClose={closeDialog}
      />}
      {dialog?.kind === "privacy" && <PrivacyDialog state={state} onClose={closeDialog} />}
      {dialog?.kind === "local-api" && <LocalApiDialog
        state={state} frozen={busy} clientKey={clientKey} clientKeyVisible={clientKeyVisible} copied={copied}
        onCopy={copyValue} onToggleKey={() => setClientKeyVisible((visible) => !visible)}
        onRotate={rotateClientKey}
        onSave={(config) => applyStateAction(() => desktopApi.saveLocalApiConfig(config))}
        onClose={closeDialog}
      />}
      {dialog?.kind === "local-api-example" && <LocalApiExamplesDialog
        api={desktopApi} endpoint={state.proxyUrl ?? localEndpoint(state.localApi)} models={state.catalog?.models ?? []}
        onCopy={(value) => desktopApi.copyText(value)} onClose={closeDialog}
      />}
      {dialog?.kind === "notifications" && <NotificationsDialog onClose={closeDialog} />}
      {dialog?.kind === "web-ui" && <WebUiDialog
        state={state}
        onSave={(config) => applyStateAction(() => desktopApi.saveWebUi(config))}
        onSetPassword={(password, currentPassword) => applyStateAction(() => desktopApi.setWebUiPassword(password, currentPassword))}
        onClose={closeDialog}
      />}
      {dialog?.kind === "usage-proof" && <UsageProofDialog activity={dialog.activity} onClose={closeDialog} />}
    </main>
  );
  return <div className="w-full h-full">{windowContent}</div>;
}
