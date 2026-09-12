import { useAgents } from "./hooks/use-agents";
import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useGatewayState } from "./lib/use-gateway-state";
import { useWindowReady } from "./lib/use-window-ready";
import { errorMessage } from "./lib/error-message";
import { RefreshCw } from "lucide-react";
import { brand } from "./generated/brand";
import { UpdateProgressDialog, useUpdates } from "./updates";
import { Button } from "./components/ui/button";
import { LocalApiExamples } from "./components/local-api-examples";
import { NotificationsSheet } from "./components/notifications";
import { Alert, AlertDescription } from "./components/ui/alert";
import type { ConfidentialProfile, ConfidentialProfileInput, GatewayState, LocalApiConfig, LaunchPreferences, RequestActivity } from "../shared/contracts";
import { MacMenuBar, PageHeader, PreviewTrayMenu, Sidebar } from "./components/navigation";
import type { SettingsTarget, View } from "./components/navigation";
import { desktopApi, previewMode } from "./lib/environment";
import { INITIAL_STATE, isProtected, protectionFlags, profileIsAvailable, unavailableState } from "./lib/protection";
import { AgentsView } from "./features/agents";
import { Overview } from "./features/overview";
import { UsageEvidenceSheet, UsageView } from "./features/usage";
import { SettingsView } from "./features/settings";
import { ProfilesSheet } from "./features/profiles";
import { PrivacyVerificationSheet } from "./features/privacy";
import { localEndpoint } from "./lib/format";
import { LocalApiSheet } from "./features/local-api";

export function App({ initialView = "overview" }: { initialView?: View }): React.JSX.Element {
  const updates = useUpdates(desktopApi, !previewMode);
  const [view, setView] = useState<View>(initialView);
  const [settingsTarget, setSettingsTarget] = useState<SettingsTarget>();
  const [profileEditorId, setProfileEditorId] = useState<string>();
  const gateway = useGatewayState(desktopApi, INITIAL_STATE);
  const state = gateway.error ? unavailableState(gateway.error) : gateway.data ?? INITIAL_STATE;
  const setState = gateway.setState;
  const stateLoaded = !gateway.isLoading;
  const [allowDevelopmentOs, setAllowDevelopmentOs] = useState(false);
  const client = useQueryClient();
  const { data: launchPreferences } = useQuery({ queryKey: ["launch-preferences"], queryFn: () => desktopApi.getLaunchPreferences() });
  const [savingPreference, setSavingPreference] = useState(false);
  const [connectingBackend, setConnectingBackend] = useState(false);
  const [actionError, setActionError] = useState<string>();
  useWindowReady(stateLoaded, desktopApi.mainWindowReady, setActionError);
  const [clientKeyError, setClientKeyError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [applying, setApplying] = useState(false);
  const [selectedUsage, setSelectedUsage] = useState<RequestActivity>();
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

  const [previewTrayOpen, setPreviewTrayOpen] = useState(false);
  const copyTimer = useRef<number | undefined>(undefined);
  const [startAfterSetup, setStartAfterSetup] = useState(false);
  const { busy, running, verified, endpointDown } = protectionFlags(state);
  const { agents, pendingAgentChanges, loadAgents, applyAgent } = useAgents(desktopApi, {
    active: view === "agents", revision: state.catalog?.revision, verified, onError: setActionError, notify,
  });
  const models = state.catalog?.models ?? [];

  useEffect(() => desktopApi.onLaunchPreferencesChange((next) => {
    void client.cancelQueries({ queryKey: ["launch-preferences"] }).then(() => client.setQueryData(["launch-preferences"], next));
  }), [client]);

  const saveLaunchPreference = async (name: keyof LaunchPreferences, enabled: boolean) => {
    setSavingPreference(true);
    setActionError(undefined);
    try { await client.cancelQueries({ queryKey: ["launch-preferences"] }); client.setQueryData(["launch-preferences"], await desktopApi.setLaunchPreference(name, enabled)); }
    catch (error) { setActionError(errorMessage(error)); }
    finally { setSavingPreference(false); }
  };

  const requestStopAllAndQuit = useCallback(async () => {
    setActionError(undefined);
    try {
      const confirmed = await desktopApi.confirm({
        title: "Stop all services and quit?",
        message: "This stops protection, restores managed agent configurations, shuts down the background service, and quits the app. In-flight requests may be interrupted.",
        confirmLabel: "Stop All and Quit",
      });
      if (confirmed) await desktopApi.stopAllAndQuit();
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }, []);

  useEffect(() => desktopApi.onStopAllRequest(() => { void requestStopAllAndQuit(); }), [requestStopAllAndQuit]);

  useEffect(() => {
    document.title = brand.productName;
  }, []);

  useLayoutEffect(() => {
    if (!previewMode) return undefined;
    const frame = document.querySelector<HTMLElement>(".desktop-window");
    if (!frame) return undefined;
    const root = document.documentElement.style;
    const sync = () => {
      const bounds = frame.getBoundingClientRect();
      root.setProperty("--window-center-x", `${bounds.left + bounds.width / 2}px`);
      root.setProperty("--window-center-y", `${bounds.top + bounds.height / 2}px`);
      root.setProperty("--window-dialog-width", `${bounds.width}px`);
      root.setProperty("--window-dialog-height", `${bounds.height}px`);
    };
    sync();
    const observer = new ResizeObserver(sync);
    observer.observe(frame);
    window.addEventListener("resize", sync);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", sync);
      root.removeProperty("--window-center-x");
      root.removeProperty("--window-center-y");
      root.removeProperty("--window-dialog-width");
      root.removeProperty("--window-dialog-height");
    };
  }, []);

  useEffect(() => {
    let active = true;
    const unsubscribeNavigate = desktopApi.onNavigate((section) => {
      if (active) {
        setSettingsTarget(undefined);
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
          setClientKeyError(undefined);
        },
        (error: unknown) => {
          if (!active || read !== keyRead) return;
          setClientKey("");
          setClientKeyError(errorMessage(error));
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
        setClientKeyError("Client key unavailable. Rotate the key again to restore access.");
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


  const run = async (action: () => Promise<GatewayState | void>, notice?: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      const next = await action();
      if (next) {
        setState(next);
      }
      if (notice) notify(notice);
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const showProfiles = (repair: boolean) => {
    if (previewMode) {
      const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
      setProfileEditorId(repair ? activeProfile?.id : undefined);
      setSettingsTarget("confidential");
      return;
    }
    void desktopApi.openNativeDialog("profiles", { repair }).catch((error: unknown) => setActionError(errorMessage(error)));
  };

  const toggleGateway = () => {
    const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
    if (!running && !busy && !state.reconnecting && !profileIsAvailable(activeProfile, state)) {
      if (state.profiles.length === 0 && !previewMode) {
        void desktopApi.openNativeDialog("setup-profile").catch((error: unknown) => setActionError(errorMessage(error)));
      } else {
        setStartAfterSetup(state.profiles.length === 0);
        showProfiles(Boolean(activeProfile));
      }
      return;
    }
    void run(() =>
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
    } catch (error) { setActionError(errorMessage(error)); }
    finally { setApplying(false); }
  };

  const saveConfiguration = async (profile: ConfidentialProfileInput, key?: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      const saved = await desktopApi.saveConfiguration(profile, !allowDevelopmentOs, key);
      setState(startAfterSetup ? await desktopApi.start(saved.config) : saved);
      setStartAfterSetup(false);
      notify(`${profile.name.trim()} saved`);
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const activateProfile = (profileId: string) => run(() => desktopApi.activateProfile(profileId));

  const deleteProfile = (profileId: string) => run(() => desktopApi.deleteProfile(profileId), "AI service profile deleted");

  const rotateClientKey = async () => {
    setActionError(undefined);
    try {
      setClientKey(await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
      notify("Client key replaced");
    } catch (error) {
      setClientKey("");
      setClientKeyVisible(false);
      setActionError(errorMessage(error));
    }
  };

  const saveLocalApi = (config: LocalApiConfig) => run(() => desktopApi.saveLocalApiConfig(config), "Local API settings saved");

  const copy = async (label: string, value: string) => {
    await run(async () => {
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
    setActionError(undefined);
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: "Reset settings?",
        message: "Stop protection, disconnect all agents and restore their configurations, and reset appearance, notifications, startup preferences, development OS policy, update channel, Local API settings, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission or uninstall the private-ai-proxy command.",
        confirmLabel: "Reset settings",
      });
    } catch (error) {
      setActionError(errorMessage(error));
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
      setActionError(errorMessage(error));
    } finally {
      setApplying(false);
    }
  };

  const problem = actionError ?? clientKeyError ?? state.error;
  const locked = applying;
  const focusPageHeading = (next: View) => {
    window.requestAnimationFrame(() => document.getElementById(`page-title-${next}`)?.focus());
  };
  const changeView = (next: View, focusHeading = true) => {
    if (next === "settings") setSettingsTarget(undefined);
    setView(next);
    if (focusHeading) focusPageHeading(next);
  };
  const openSettings = (target: SettingsTarget) => {
    if (target === "confidential") {
      showProfiles(false);
      return;
    }
    if (!previewMode) {
      void desktopApi.openNativeDialog(target).catch((error: unknown) => setActionError(errorMessage(error)));
      return;
    }
    setSettingsTarget(target);
  };
  const inspectUsage = useCallback((activity: RequestActivity) => {
    if (previewMode) {
      setSelectedUsage(activity);
      return;
    }
    void desktopApi.openNativeDialog("usage-proof", { recordId: activity.id }).catch((error: unknown) => setActionError(errorMessage(error)));
  }, []);

  const windowContent = (
    <main className="app-shell w-full h-full grid grid-cols-[var(--sidebar-width)_minmax(0,_1fr)] overflow-hidden bg-background max-[780px]:grid-cols-[154px_minmax(0,_1fr)] max-[620px]:grid-cols-[68px_minmax(0,_1fr)] max-[440px]:grid-cols-[56px_minmax(0,_1fr)]">
      <Sidebar view={view} previewControls={previewMode} updateAvailable={Boolean(updates.info?.version)} updateBusy={Boolean(updates.busy)} onInstallUpdate={() => void updates.install()} onChange={changeView} />
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
        {state.backendConnected === false && <Alert>
          <AlertDescription className="flex items-center justify-between gap-4">
            <span>Backend disconnected</span>
            <Button disabled={connectingBackend} onClick={() => {
              setConnectingBackend(true);
              void desktopApi.startBackendService().then((next) => {
                setState(next);
                setActionError(undefined);
              }).catch((error: unknown) => setActionError(errorMessage(error)))
                .finally(() => setConnectingBackend(false));
            }}><RefreshCw aria-hidden="true" />{connectingBackend ? "Connecting" : "Start backend"}</Button>
          </AlertDescription>
        </Alert>}
        {view === "overview" && (
          <Overview
            pendingAgentChanges={pendingAgentChanges}
            state={state}
            agents={agents}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            developmentMode={allowDevelopmentOs}
            problem={problem}
            locked={locked}
            clientKey={clientKey}
            clientKeyVisible={clientKeyVisible}
            copied={copied}
            onToggle={toggleGateway}
            onSettings={() => openSettings("confidential")}
            onPrivacy={() => openSettings("privacy")}
            onLocalSettings={() => openSettings("local-api")}
            onLocalExamples={() => openSettings("local-api-example")}
            onAgents={() => changeView("agents")}
            onUsage={() => changeView("usage")}
            onCopy={copy}
            onToggleClientKey={() => setClientKeyVisible((visible) => !visible)}
            onSelect={(agent, connect) => void applyAgent(agent, connect)}
            onInspect={inspectUsage}
          />
        )}
        {view === "agents" && (
          <AgentsView
            pendingAgentChanges={pendingAgentChanges}
            agents={agents}
            locked={locked}
            problem={problem}
            onSelect={(agent, connect) => void applyAgent(agent, connect)}
          />
        )}
        {view === "usage" && (
          <UsageView
            state={state}
            agents={agents}
            problem={problem}
            onInspect={inspectUsage}
          />
        )}
        {view === "settings" && (
          <SettingsView
            updates={updates}
            state={state}
            busy={busy}
            running={running}
            allowDevelopmentOs={allowDevelopmentOs}
            locked={locked || Object.keys(pendingAgentChanges).length > 0}
            problem={problem}
            onPolicy={(value) => void changeDevelopmentOs(value)}
            onResetSettings={() => void resetSettings()}
            onAboutLink={(target) => void run(() => desktopApi.openAboutLink(target))}
            onOpen={openSettings}
            launchPreferences={launchPreferences}
            savingPreference={savingPreference}
            onLaunchPreference={(name, enabled) => void saveLaunchPreference(name, enabled)}
          />
        )}
        </div>
      </section>

      {settingsTarget === "confidential" && (
        <ProfilesSheet
          state={state}
          busy={busy}
          running={running}
          initialEditorProfileId={profileEditorId}
          startAfterSave={startAfterSetup}
          onSave={saveConfiguration}
          onActivate={activateProfile}
          onDelete={deleteProfile}
          onClose={() => {
            setStartAfterSetup(false);
            setProfileEditorId(undefined);
            setSettingsTarget(undefined);
          }}
        />
      )}
      {settingsTarget === "privacy" && (
        <PrivacyVerificationSheet state={state} onClose={() => setSettingsTarget(undefined)} />
      )}
      {settingsTarget === "notifications" && <NotificationsSheet onClose={() => setSettingsTarget(undefined)} />}
      {settingsTarget === "local-api-example" && <LocalApiExamples
        api={desktopApi}
        endpoint={state.proxyUrl ?? localEndpoint(state.localApi)}
        models={models} onCopy={(value) => desktopApi.copyText(value)} onClose={() => setSettingsTarget(undefined)}
      />}
      {settingsTarget === "local-api" && (
        <LocalApiSheet
          state={state}
          frozen={busy}
          clientKey={clientKey}
          clientKeyVisible={clientKeyVisible}
          copied={copied}
          onCopy={copy}
          onToggleKey={() => setClientKeyVisible((visible) => !visible)}
          onRotate={rotateClientKey}
          onSave={saveLocalApi}
          onClose={() => setSettingsTarget(undefined)}
        />
      )}
      {selectedUsage && (
        <UsageEvidenceSheet activity={selectedUsage} onClose={() => setSelectedUsage(undefined)} />
      )}
      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {notice?.text}
      </div>
      <UpdateProgressDialog updates={updates} />
    </main>
  );

  if (!previewMode) {
    return <div className="native-host w-full h-full">{windowContent}</div>;
  }

  return (
    <div className="desktop-preview relative w-full h-full min-w-50 pt-12 pr-6 pb-6 pl-6 grid place-items-center overflow-hidden bg-background bg-[url('/macos-wallpaper.webp')] bg-center bg-cover bg-no-repeat max-[620px]:pt-10 max-[620px]:pr-2 max-[620px]:pb-2 max-[620px]:pl-2">
      <MacMenuBar protected={isProtected(state)} trayOpen={previewTrayOpen} onTray={() => setPreviewTrayOpen((open) => !open)} />
      <div className="desktop-window relative box-content w-[min(1052px,_calc(100%_-_2px))] h-[min(752px,_calc(100vh_-_74px))] min-h-140 overflow-hidden bg-background border border-[color-mix(in_srgb,_var(--color-black)_20%,_transparent)] rounded-lg [box-shadow:0_22px_60px_color-mix(in_srgb,_var(--color-black)_30%,_transparent),_0_2px_8px_color-mix(in_srgb,_var(--color-black)_16%,_transparent)] max-[620px]:w-[calc(100vw_-_16px)] max-[620px]:h-[calc(100vh_-_48px)] max-[620px]:min-h-0">{windowContent}</div>
      {previewTrayOpen && (
        <PreviewTrayMenu
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
          developmentMode={allowDevelopmentOs}
          openAtLogin={launchPreferences?.openAtLogin ?? false}
          onProtection={toggleGateway}
          onOpen={() => setPreviewTrayOpen(false)}
          onSettings={() => {
            setPreviewTrayOpen(false);
            changeView("settings");
          }}
          onOpenAtLogin={() => void saveLaunchPreference("openAtLogin", !launchPreferences?.openAtLogin)}
          onQuit={() => {
            setPreviewTrayOpen(false);
            notify("Quit is available in the installed macOS app");
          }}
          onStopAllQuit={() => {
            setPreviewTrayOpen(false);
            void requestStopAllAndQuit();
          }}
        />
      )}
    </div>
  );
}
