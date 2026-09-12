import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useGatewayState } from "../lib/use-gateway-state";
import { useWindowReady } from "../lib/use-window-ready";
import { errorMessage } from "../lib/error-message";
import { TriangleAlert } from "lucide-react";
import { initialGatewayState } from "../desktop-api";
import { brand } from "../generated/brand";
import { UpdateProgressMeter } from "../updates";
import type { UpdateProgress } from "../../shared/contracts";
import { Button } from "../components/ui/button";
import { LocalApiExamples } from "../components/local-api-examples";
import { NotificationsProvider, NotificationsSheet, useNotifications } from "../components/notifications";
import { useDialogClose } from "../components/dialog-close";
import { NativeDialogHost } from "../components/sheet";
import type { GatewayState, LocalApiConfig } from "../../shared/contracts";
import { desktopApi, previewMode, query } from "../lib/environment";
import { INITIAL_STATE, protectionFlags } from "../lib/protection";
import { ProfileEditorSheet, ProfilesSheet } from "../features/profiles";
import { localEndpoint } from "../lib/format";
import { PrivacyVerificationSheet } from "../features/privacy";
import { LocalApiSheet } from "../features/local-api";
import { UsageEvidenceSheet } from "../features/usage";

const NativeStateContext = createContext<GatewayState | undefined>(initialGatewayState);

function useNativeGatewayWindow(title: string, contentReady = true): {
  state: GatewayState;
  setState: React.Dispatch<React.SetStateAction<GatewayState>>;
  loaded: boolean;
  loadError?: string;
  closed: boolean;
  close(): void;
} {
  const initialState = useContext(NativeStateContext);
  const gateway = useGatewayState(desktopApi, initialState ?? INITIAL_STATE);
  const state = gateway.data ?? initialState ?? INITIAL_STATE;
  const setState = gateway.setState;
  const loaded = Boolean(initialState) || !gateway.isLoading;
  const [presentationError, setLoadError] = useState<string>();
  const loadError = presentationError ?? (gateway.error ? errorMessage(gateway.error) : undefined);
  const [closed, setClosed] = useState(false);
  useWindowReady(loaded && (contentReady || Boolean(loadError)) && !closed, desktopApi.nativeDialogReady, setLoadError);

  useEffect(() => {
    document.title = `${title} - ${brand.productName}`;
    const root = document.documentElement;
    root.classList.add("is-native-dialog");
    return () => { root.classList.remove("is-native-dialog"); };
  }, [title]);

  const close = () => {
    if (previewMode) {
      setClosed(true);
      return;
    }
    void desktopApi.closeNativeDialog().catch((error: unknown) => setLoadError(errorMessage(error)));
  };
  return { state, setState, loaded, loadError, closed, close };
}

function NativeUpdateProgressWindow(): React.JSX.Element {
  const [progress, setProgress] = useState<UpdateProgress>();
  const native = useNativeGatewayWindow("Software Update", Boolean(progress));
  useDialogClose(native.close, Boolean(progress?.error), !native.closed);
  useEffect(() => desktopApi.onUpdateProgress(setProgress), []);
  if (native.closed) return <main aria-label="Software update closed" />;
  return <NativeDialogHost className="p-6 flex flex-col gap-4" aria-labelledby="update-title">
    <h2 id="update-title" className="text-lg font-semibold">{progress?.error ? "Update failed" : "Installing update"}</h2>
    <p className="min-h-0 overflow-auto break-words text-sm text-muted-foreground" role={progress?.error ? "alert" : undefined}>{progress?.error ?? "The app will restart when installation completes."}</p>
    {!progress?.error && <UpdateProgressMeter progress={progress} />}
    {native.loadError && <p role="alert" className="text-sm text-destructive">{native.loadError}</p>}
    {progress?.error && <div className="mt-auto flex justify-end"><Button variant="outline" onClick={native.close}>Done</Button></div>}
  </NativeDialogHost>;
}

function NativeDialogStatus({ label, error, onClose }: { label: string; error?: string; onClose(): void }): React.JSX.Element | null {
  useDialogClose(onClose, Boolean(error), Boolean(error));
  // The native window remains hidden until content or an actionable error is ready.
  if (!error) return null;
  return (
    <NativeDialogHost className="native-dialog-loading flex flex-col items-center justify-center gap-3 p-5 text-center text-muted-foreground" aria-label={label}>
      <TriangleAlert aria-hidden="true" />
      <span className="min-h-0 overflow-auto break-words" role="alert">{error}</span>
      <Button variant="outline" onClick={onClose}>Done</Button>
    </NativeDialogHost>
  );
}

function NativeProfilesWindow({ repair, editor = false, profileId, startAfterSave = false }: { repair: boolean; editor?: boolean; profileId?: string | null; startAfterSave?: boolean }): React.JSX.Element {
  const native = useNativeGatewayWindow(editor ? profileId ? "Edit Profile" : "New Profile" : "Profiles");
  const [actionError, setActionError] = useState<string>();
  const [repairRequest, setRepairRequest] = useState(repair ? 1 : 0);
  useEffect(() => desktopApi.onProfileRepairRequest(() => setRepairRequest((current) => current + 1)), []);
  const run = async (action: () => Promise<GatewayState>): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      native.setState(await action());
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  if (native.closed) return <NativeDialogHost  aria-label="Profiles closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="profiles" error={native.loadError} onClose={native.close} />;
  const { busy, running } = protectionFlags(native.state);
  const editingProfileId = profileId;
  const editingProfile = native.state.profiles.find((profile) => profile.id === editingProfileId);
  if (editor && editingProfileId && !editingProfile) return <NativeDialogStatus label="profile" error="This profile is no longer available." onClose={native.close} />;
  if (editor) return <NativeDialogHost ><ProfileEditorSheet
    state={native.state} busy={busy} running={running}
    profile={editingProfile}
    startAfterSave={startAfterSave}
    onSave={(profile, key) => run(async () => {
      const saved = await desktopApi.saveConfiguration(profile, native.state.config.requireProductionOs, key);
      return startAfterSave ? desktopApi.start(saved.config) : saved;
    })}
    onDelete={(profileId) => run(() => desktopApi.deleteProfile(profileId))}
    onComplete={native.close} onDeleted={native.close} onClose={native.close}
  /></NativeDialogHost>;
  return (
    <NativeDialogHost >
      {actionError && <div className="sr-only" role="alert">{actionError}</div>}
      <ProfilesSheet
        key={repairRequest}
        state={native.state}
        busy={busy}
        running={running}
        initialEditorProfileId={repairRequest ? native.state.activeProfileId || undefined : undefined}
        onSave={(profile, key) => run(() => desktopApi.saveConfiguration(profile, native.state.config.requireProductionOs, key))}
        onActivate={(profileId) => run(() => desktopApi.activateProfile(profileId))}
        onDelete={(profileId) => run(() => desktopApi.deleteProfile(profileId))}
        onClose={native.close}
      />
    </NativeDialogHost>
  );
}

function NativeNotificationsWindow(): React.JSX.Element {
  const { data, error } = useNotifications();
  const native = useNativeGatewayWindow("Notifications", Boolean(data || error));
  if (native.closed) return <main aria-label="Notifications closed" />;
  return <NativeDialogHost ><NotificationsSheet onClose={native.close} /></NativeDialogHost>;
}

function NativeLocalApiExampleWindow(): React.JSX.Element {
  const [exampleReady, setExampleReady] = useState(false);
  const native = useNativeGatewayWindow("Local API examples", exampleReady);
  if (native.closed) return <NativeDialogHost  aria-label="Local API examples closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="Local API examples" error={native.loadError} onClose={native.close} />;
  return <NativeDialogHost ><LocalApiExamples
    api={desktopApi}
    onReady={() => setExampleReady(true)}
    endpoint={native.state.proxyUrl ?? localEndpoint(native.state.localApi)}
    models={native.state.catalog?.models ?? []}
    onCopy={(value) => desktopApi.copyText(value)} onClose={native.close}
  /></NativeDialogHost>;
}

function NativePrivacyWindow(): React.JSX.Element {
  const native = useNativeGatewayWindow("Privacy Verification");
  if (native.closed) return <NativeDialogHost  aria-label="Privacy verification closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="privacy verification" error={native.loadError} onClose={native.close} />;
  return (
    <NativeDialogHost >
      <PrivacyVerificationSheet state={native.state} onClose={native.close} />
    </NativeDialogHost>
  );
}

function NativeLocalApiWindow(): React.JSX.Element {
  const [clientKey, setClientKey] = useState("");
  const [keyLoaded, setKeyLoaded] = useState(false);
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [copied, setCopied] = useState<string>();
  const [keyError, setKeyError] = useState<string>();
  const [actionError, setActionError] = useState<string>();
  const native = useNativeGatewayWindow("Local API Settings", keyLoaded);
  const copyTimer = useRef<number | undefined>(undefined);

  const loadClientKey = useCallback(() => {
    void desktopApi.getClientKey().then(
      (key) => {
        setClientKey(key);
        setKeyError(undefined);
        setKeyLoaded(true);
      },
      (error: unknown) => {
        setKeyError(errorMessage(error));
        setKeyLoaded(true);
      },
    );
  }, []);
  useEffect(() => {
    loadClientKey();
    const unsubscribe = desktopApi.onClientKeyChange((available) => {
      if (available) loadClientKey();
      else {
        setClientKey("");
        setClientKeyVisible(false);
        setActionError("Client key unavailable. Rotate the key again to restore access.");
      }
    });
    return () => {
      unsubscribe();
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
    };
  }, [loadClientKey]);

  if (native.closed) return <NativeDialogHost  aria-label="Local API settings closed" />;
  if (!native.loaded || !keyLoaded || native.loadError || keyError) {
    return <NativeDialogStatus label="Local API settings" error={native.loadError ?? keyError} onClose={native.close} />;
  }
  const { busy } = protectionFlags(native.state);
  const copy = async (label: string, value: string) => {
    setActionError(undefined);
    try {
      await desktopApi.copyText(value);
      setCopied(label);
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(undefined), 1_400);
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };
  const rotate = async () => {
    setActionError(undefined);
    try {
      setClientKey(await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
    } catch (error) {
      setClientKey("");
      setClientKeyVisible(false);
      setActionError(errorMessage(error));
    }
  };
  const saveLocalApi = async (config: LocalApiConfig): Promise<string | undefined> => {
    try {
      setActionError(undefined);
      native.setState(await desktopApi.saveLocalApiConfig(config));
      return undefined;
    } catch (error) {
      return errorMessage(error);
    }
  };
  return (
    <NativeDialogHost >
      <LocalApiSheet
        state={native.state}
        frozen={busy}
        clientKey={clientKey}
        clientKeyVisible={clientKeyVisible}
        copied={copied}
        externalError={actionError}
        onCopy={copy}
        onToggleKey={() => setClientKeyVisible((visible) => !visible)}
        onRotate={rotate}
        onSave={saveLocalApi}
        onClose={native.close}
      />
    </NativeDialogHost>
  );
}


function NativeUsageProofWindow({ initialRecordId }: { initialRecordId: string }): React.JSX.Element {
  const [recordId, setRecordId] = useState(initialRecordId);
  const initialState = useContext(NativeStateContext);
  const { data: activity, error: recordError } = useQuery({
    queryKey: ["usage-record", recordId], queryFn: () => desktopApi.getUsageRecord(recordId),
    initialData: initialState?.activity.find((item) => item.id === recordId),
  });
  const error = recordError ? errorMessage(recordError) : undefined;
  const native = useNativeGatewayWindow("Usage Proof", Boolean(activity || error));
  useEffect(() => desktopApi.onUsageProofRequest(setRecordId), []);
  if (native.closed) return <NativeDialogHost  aria-label="Usage proof closed" />;
  if (!activity || error || native.loadError) return <NativeDialogStatus label="usage proof" error={error ?? native.loadError} onClose={native.close} />;
  return <NativeDialogHost ><UsageEvidenceSheet activity={activity} onClose={native.close} /></NativeDialogHost>;
}

export function NativeWindowContent(): React.JSX.Element | null {
  const nativeDialog = query.get("native-dialog");
  const [request, setRequest] = useState<{ state?: GatewayState; repair: boolean; recordId?: string | null; profileId?: string | null; startAfterSave?: boolean } | null>(() => ({
    state: initialGatewayState, repair: query.get("repair") === "1",
    recordId: query.get("record"), profileId: query.get("profile"), startAfterSave: query.get("start") === "1",
  }));
  const [generation, setGeneration] = useState(0);
  useEffect(() => {
    const opened = desktopApi.onNativeDialogOpen((next) => {
      setRequest(next);
      setGeneration((value) => value + 1);
    });
    // Unmount forms when hidden so drafts, credentials, and subscriptions do not persist.
    const dismissed = desktopApi.onNativeDialogDismissed(() => setRequest(null));
    return () => { opened(); dismissed(); };
  }, []);
  if (!request) return null;
  const content = nativeDialog === "profiles" ? <NativeProfilesWindow repair={request.repair} />
    : nativeDialog === "update-progress" ? <NativeUpdateProgressWindow />
    : nativeDialog === "local-api-example" ? <NativeLocalApiExampleWindow />
    : nativeDialog === "notifications" ? <NativeNotificationsWindow />
    : nativeDialog === "profile-editor" ? <NativeProfilesWindow repair={false} editor profileId={request.profileId} startAfterSave={request.startAfterSave} />
    : nativeDialog === "privacy" ? <NativePrivacyWindow />
      : nativeDialog === "local-api" ? <NativeLocalApiWindow />
        : nativeDialog === "usage-proof" ? <NativeUsageProofWindow initialRecordId={request.recordId ?? ""} />
          : null;
  return <NativeStateContext.Provider key={generation} value={request.state}><NotificationsProvider api={desktopApi}>{content}</NotificationsProvider></NativeStateContext.Provider>;
}
