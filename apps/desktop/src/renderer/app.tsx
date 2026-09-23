import { useAgents } from "./hooks/use-agents";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cliRegistrationQuery, usagePageQuery } from "./lib/page-queries";
import { useGatewayState } from "./lib/use-gateway-state";
import { useWindowReady } from "./lib/use-window-ready";
import { errorMessage } from "./lib/error-message";
import { showErrorAlert } from "./lib/error-alert";
import { brand } from "./generated/brand";
import { useUpdates } from "./updates";
import type { AgentStatus, ConfidentialProfile, GatewayState, LaunchPreferences, RequestActivity, SurfaceErrorScope } from "../shared/contracts";
import { PageHeader, Sidebar } from "./components/navigation";
import type { SettingsTarget, View } from "./components/navigation";
import { desktopApi, distributionCapabilities } from "./lib/environment";
import { INITIAL_STATE, protectionFlags, profileIsAvailable, unavailableState } from "./lib/protection";
import { AgentsView } from "./features/agents";
import { Overview } from "./features/overview";
import { UsageView } from "./features/usage";
import { SettingsView } from "./features/settings";

const errorTitles: Record<SurfaceErrorScope, string> = {
  agents: "Agent action failed", protection: "Protection action failed", profiles: "Profile action failed",
  "local-api": "Local API action failed", usage: "Usage action failed", settings: "Settings action failed",
};

export function App({ initialView = "overview" }: { initialView?: View }): React.JSX.Element {
  const updates = useUpdates(desktopApi, distributionCapabilities.nativeUpdates || distributionCapabilities.channel === "web");
  const [view, setView] = useState<View>(initialView);
  const gateway = useGatewayState(desktopApi, INITIAL_STATE);
  const state = gateway.error ? unavailableState(gateway.error) : gateway.data ?? INITIAL_STATE;
  const setState = gateway.setState;
  const stateLoaded = !gateway.isLoading;
  const [allowDevelopmentOs, setAllowDevelopmentOs] = useState(false);
  const client = useQueryClient();
  const backendReady = Boolean(gateway.data) && state.backendConnected !== false;
  useEffect(() => {
    if (!backendReady) return;
    // Prefetch shares page caches; failures are presented when the page is opened.
    void client.prefetchQuery(usagePageQuery());
    if (distributionCapabilities.cliRegistration) void client.prefetchQuery(cliRegistrationQuery());
  }, [client, backendReady]);
  const { data: launchPreferences } = useQuery({ queryKey: ["launch-preferences"], queryFn: () => desktopApi.getLaunchPreferences() });
  const [savingPreference, setSavingPreference] = useState(false);
  const [connectingBackend, setConnectingBackend] = useState(false);
  const reportSurfaceError = useCallback((scope: SurfaceErrorScope, error: unknown) => {
    void showErrorAlert(errorTitles[scope], error);
  }, []);
  const reportWindowError = useCallback((message: string) => reportSurfaceError("settings", message), [reportSurfaceError]);
  useWindowReady(stateLoaded, desktopApi.mainWindowReady, reportWindowError);
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [applying, setApplying] = useState(false);
  const [notice, setNotice] = useState<{ id: number; text: string } | undefined>(() => initialView === "settings" ? { id: Date.now(), text: "Settings reset" } : undefined);
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
  useEffect(() => desktopApi.onSurfaceError(({ scope, message }) => {
    reportSurfaceError(scope, message);
  }), [reportSurfaceError]);

  const saveLaunchPreference = async (name: keyof LaunchPreferences, enabled: boolean) => {
    setSavingPreference(true);
    try { await client.cancelQueries({ queryKey: ["launch-preferences"] }); client.setQueryData(["launch-preferences"], await desktopApi.setLaunchPreference(name, enabled)); }
    catch (error) { reportSurfaceError("settings", error); }
    finally { setSavingPreference(false); }
  };

  const requestStopAllAndQuit = useCallback(async () => {
    try {
      const confirmed = await desktopApi.confirm({
        title: "Stop all services and quit?",
        message: agentAccessStatus === "authorized"
          ? "This stops protection, restores managed agent configurations, shuts down the background service, and quits the app. In-flight requests may be interrupted."
          : "This stops protection, shuts down the background service, and quits the app. In-flight requests may be interrupted.",
        confirmLabel: "Stop All and Quit",
      });
      if (confirmed) await desktopApi.stopAllAndQuit();
    } catch (error) {
      reportSurfaceError("settings", error);
    }
  }, [agentAccessStatus, reportSurfaceError]);

  useEffect(() => desktopApi.onStopAllRequest(() => { void requestStopAllAndQuit(); }), [requestStopAllAndQuit]);

  useEffect(() => {
    document.title = brand.productName;
  }, []);

  useEffect(() => {
    let active = true;
    const unsubscribeNavigate = desktopApi.onNavigate((section) => {
      if (active) {
        setView(section);
        window.requestAnimationFrame(() => document.getElementById(`page-title-${section}`)?.focus());
      }
    });
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
      unsubscribeNavigate();
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
      if (document.querySelector("dialog[open]")) return;
      event.preventDefault();
      setView("settings");
      window.requestAnimationFrame(() => document.getElementById("page-title-settings")?.focus());
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, []);


  const applyStateAction = async (action: () => Promise<GatewayState | void>, notice?: string): Promise<string | undefined> => {
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

  const runAction = async (scope: SurfaceErrorScope, action: () => Promise<GatewayState | void>, notice?: string) => {
    const message = await applyStateAction(action, notice);
    if (message) reportSurfaceError(scope, message);
    return message;
  };

  const showProfiles = (repair: boolean) => {
    const scope = view === "settings" ? "settings" : "profiles";
    void desktopApi.openNativeDialog("profiles", { repair }).catch((error: unknown) => reportSurfaceError(scope, error));
  };

  const toggleGateway = () => {
    const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
    if (!running && !busy && !state.reconnecting && !profileIsAvailable(activeProfile, state)) {
      if (state.profiles.length === 0) {
        void desktopApi.openNativeDialog("setup-profile").catch((error: unknown) => reportSurfaceError("profiles", error));
      } else {
        showProfiles(Boolean(activeProfile));
      }
      return;
    }
    void runAction("protection", () =>
      running || busy || state.reconnecting ? desktopApi.stop() : desktopApi.start({ remoteUrl: state.config.remoteUrl, requireProductionOs: !allowDevelopmentOs }),
    );
  };

  const changeDevelopmentOs = async (enabled: boolean) => {
    if (applying) return;
    setApplying(true);
    try {
      if (!await desktopApi.confirm({ title: enabled ? "Allow development OS?" : "Require production OS?", message: "Protection will stop before changing this policy.", confirmLabel: "Stop and Change" })) return;
      setState(await desktopApi.stop());
      setAllowDevelopmentOs(enabled);
    } catch (error) { reportSurfaceError("settings", error); }
    finally { setApplying(false); }
  };

  const copy = async (label: string, value: string) => {
    await runAction("local-api", async () => {
      await desktopApi.copyText(value);
      setCopied(label);
      notify(`${label} copied`);
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(
        () => setCopied((current) => (current === label ? undefined : current)),
        1_400,
      );
    });
  };

  const resetSettings = async () => {
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: "Reset settings?",
        message: agentAccessStatus === "authorized"
          ? "Stop protection, disconnect all agents and restore their configurations, and reset appearance, notifications, startup preferences, development OS policy, update channel, Local API settings, web UI, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission or uninstall the private-ai-proxy command."
          : "Stop protection and reset appearance, notifications, startup preferences, development OS policy, Local API settings, web UI, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission.",
        confirmLabel: "Reset settings",
      });
    } catch (error) {
      reportSurfaceError("settings", error);
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
      reportSurfaceError("settings", error);
    } finally {
      setApplying(false);
    }
  };

  const locked = applying;
  const focusPageHeading = (next: View) => {
    window.requestAnimationFrame(() => document.getElementById(`page-title-${next}`)?.focus());
  };
  const changeView = (next: View, focusHeading = true) => {
    setView(next);
    if (focusHeading) focusPageHeading(next);
  };
  const openSettings = (target: SettingsTarget) => {
    if (target === "confidential") {
      if (state.profiles.length === 0) {
        void desktopApi.openNativeDialog("setup-profile").catch((error: unknown) => reportSurfaceError("profiles", error));
      } else {
        showProfiles(false);
      }
      return;
    }
    const scope: SurfaceErrorScope = view === "settings" ? "settings" : target === "privacy" ? "protection" : target === "local-api" || target === "local-api-example" ? "local-api" : "settings";
    void desktopApi.openNativeDialog(target).catch((error: unknown) => reportSurfaceError(scope, error));
  };
  const inspectUsage = useCallback((activity: RequestActivity) => {
    void desktopApi.openNativeDialog("usage-proof", { recordId: activity.id }).catch((error: unknown) => reportSurfaceError("usage", error));
  }, [reportSurfaceError]);

  const selectAgent = (agent: AgentStatus, connect: boolean) => {
    void applyAgent(agent, connect);
  };

  const startBackend = async () => {
    if (connectingBackend) return;
    setConnectingBackend(true);
    try { setState(await desktopApi.startBackendService()); }
    catch (error) { reportSurfaceError("protection", error); }
    finally { setConnectingBackend(false); }
  };

  const windowContent = (
    <main className="app-shell w-full h-full grid grid-cols-[var(--sidebar-width)_minmax(0,_1fr)] overflow-hidden bg-background max-[780px]:grid-cols-[154px_minmax(0,_1fr)] max-[620px]:grid-cols-[68px_minmax(0,_1fr)] max-[440px]:grid-cols-[56px_minmax(0,_1fr)]">
      <Sidebar view={view} updateReady={updates.ready} updateBusy={Boolean(updates.busy)} onRestartUpdate={() => void updates.restart()} onChange={changeView} />
      <section className="workspace min-w-0 min-h-0 flex flex-col">
        <PageHeader
          view={view}
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
          developmentMode={allowDevelopmentOs}
          onToggle={toggleGateway}
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
            onToggle={toggleGateway}
            onStartBackend={() => void startBackend()}
            onSettings={() => openSettings("confidential")}
            onPrivacy={() => openSettings("privacy")}
            onLocalSettings={() => openSettings("local-api")}
            onLocalExamples={() => openSettings("local-api-example")}
            onAgents={() => changeView("agents")}
            onUsage={() => changeView("usage")}
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
            onAboutLink={(target) => void runAction("settings", () => desktopApi.openAboutLink(target))}
            onOpen={openSettings}
            launchPreferences={launchPreferences}
            savingPreference={savingPreference}
            onLaunchPreference={(name, enabled) => void saveLaunchPreference(name, enabled)}
          />
        )}
        </div>
      </section>

      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {notice?.text}
      </div>
    </main>
  );
  return <div className="native-host w-full h-full">{windowContent}</div>;
}
