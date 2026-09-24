import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useAppState } from "../lib/use-app-state";
import { useWindowReady } from "../lib/use-window-ready";
import { errorMessage } from "../lib/error-message";
import { showErrorAlert, useErrorAlert } from "../lib/error-alert";
import { brand } from "../generated/brand";
import { LocalApiExamples } from "../components/local-api-examples";
import { NotificationsProvider, NotificationsSheet, useNotifications } from "../components/notifications";
import { NativeDialogHost } from "../components/sheet";
import type { AppState, ListenConfig, WebUiConfig } from "../../shared/contracts";
import { desktopApi, initialAppState, query, web } from "../lib/environment";
import { INITIAL_STATE, protectionFlags } from "../lib/protection";
import { ProfileEditorSheet, ProfilesSheet } from "../features/profiles";
import { localEndpoint } from "../lib/format";
import { PrivacyVerificationSheet } from "../features/privacy";
import { LocalApiSheet } from "../features/local-api";
import { WebUiSheet } from "../features/web-ui";
import { UsageEvidenceSheet } from "../features/usage";

const NativeStateContext = createContext<AppState | undefined>(initialAppState);

type NativeWindowOptions = {
  contentReady?: boolean;
  contentError?: string;
  validate?(state: AppState): string | undefined;
};

type NativeWindowRequest = {
  kind?: string;
  state?: AppState;
  repair: boolean;
  recordId?: string | null;
  profileId?: string | null;
  startAfterSave?: boolean;
};

function useNativeWindow(title: string, options: NativeWindowOptions = {}): {
  state: AppState;
  setState: React.Dispatch<React.SetStateAction<AppState>>;
  loaded: boolean;
  loadError?: string;
  close(): void;
} {
  const initialState = useContext(NativeStateContext);
  const appState = useAppState(desktopApi, initialState ?? INITIAL_STATE);
  const state = appState.data ?? initialState ?? INITIAL_STATE;
  const setState = appState.setState;
  const loaded = Boolean(initialState) || !appState.isLoading;
  const [presentationError, setLoadError] = useState<string>();
  const validationError = loaded ? options.validate?.(state) : undefined;
  const loadError = presentationError ?? options.contentError ?? validationError ?? (appState.error ? errorMessage(appState.error) : undefined);
  useWindowReady(loaded && (options.contentReady ?? true) && !loadError, desktopApi.nativeDialogReady, setLoadError);

  useEffect(() => {
    if (web) return;
    document.title = `${title} - ${brand.productName}`;
    const root = document.documentElement;
    root.classList.add("is-native-dialog");
    return () => { root.classList.remove("is-native-dialog"); };
  }, [title]);

  const close = useCallback(() => {
    void desktopApi.closeNativeDialog().catch((error: unknown) => setLoadError(errorMessage(error)));
  }, []);
  return { state, setState, loaded, loadError, close };
}

function NativeDialogStatus({ label, error, onClose }: { label: string; error?: string; onClose(): void }): React.JSX.Element | null {
  const reported = useRef(false);
  useEffect(() => {
    if (!error || reported.current) return;
    reported.current = true;
    void showErrorAlert(`Could not open ${label}`, error).then(onClose);
  }, [error, label, onClose]);
  return null;
}

function NativeProfilesWindow({ repair, editor = false, profileId, startAfterSave = false }: { repair: boolean; editor?: boolean; profileId?: string | null; startAfterSave?: boolean }): React.JSX.Element {
  const native = useNativeWindow(editor ? profileId ? "Edit Profile" : "New Profile" : "Profiles", {
    validate: (state) => editor && profileId && !state.profiles.some((profile) => profile.id === profileId)
      ? "This profile is no longer available."
      : undefined,
  });
  const [repairRequest, setRepairRequest] = useState(repair ? 1 : 0);
  useEffect(() => desktopApi.onProfileRepairRequest(() => setRepairRequest((current) => current + 1)), []);
  const run = async (action: () => Promise<AppState>): Promise<string | undefined> => {
    try {
      native.setState(await action());
      return undefined;
    } catch (error) {
      return errorMessage(error);
    }
  };

  if (!native.loaded || native.loadError) return <NativeDialogStatus label="profiles" error={native.loadError} onClose={native.close} />;
  const { busy, running } = protectionFlags(native.state);
  const editingProfileId = profileId;
  const editingProfile = native.state.profiles.find((profile) => profile.id === editingProfileId);
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
      <ProfilesSheet
        key={repairRequest}
        state={native.state}
        busy={busy}
        initialEditorProfileId={repairRequest ? native.state.activeProfileId || undefined : undefined}
        onActivate={(profileId) => run(() => desktopApi.activateProfile(profileId))}
        onClose={native.close}
      />
    </NativeDialogHost>
  );
}

function NativeNotificationsWindow(): React.JSX.Element {
  const { data, error } = useNotifications();
  const native = useNativeWindow("Notifications", {
    contentReady: Boolean(data),
    contentError: !data ? error : undefined,
  });
  if (native.loadError) return <NativeDialogStatus label="notifications" error={native.loadError} onClose={native.close} />;
  return <NativeDialogHost ><NotificationsSheet onClose={native.close} /></NativeDialogHost>;
}

function NativeLocalApiExampleWindow(): React.JSX.Element {
  const [exampleReady, setExampleReady] = useState(false);
  const [exampleError, setExampleError] = useState<string>();
  const exampleLoaded = useCallback((error?: string) => {
    setExampleError(error);
    setExampleReady(true);
  }, []);
  const native = useNativeWindow("Local API examples", { contentReady: exampleReady, contentError: exampleError });
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="Local API examples" error={native.loadError} onClose={native.close} />;
  return <NativeDialogHost ><LocalApiExamples
    api={desktopApi}
    onReady={exampleLoaded}
    endpoint={native.state.proxyUrl ?? localEndpoint(native.state.localApi)}
    models={native.state.catalog?.models ?? []}
    onCopy={(value) => desktopApi.copyText(value)} onClose={native.close}
  /></NativeDialogHost>;
}

function NativePrivacyWindow(): React.JSX.Element {
  const native = useNativeWindow("Privacy Verification");
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
  const reportError = useErrorAlert("Local API action failed");
  const rotatingClientKey = useRef(false);
  const native = useNativeWindow("Local API Settings", { contentReady: keyLoaded, contentError: keyError });
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
        if (!rotatingClientKey.current) reportError("Client key unavailable. Rotate the key again to restore access.");
      }
    });
    return () => {
      unsubscribe();
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
    };
  }, [loadClientKey, reportError]);

  if (!native.loaded || !keyLoaded || native.loadError) {
    return <NativeDialogStatus label="Local API settings" error={native.loadError} onClose={native.close} />;
  }
  const { busy } = protectionFlags(native.state);
  const copy = async (label: string, value: string) => {
    try {
      await desktopApi.copyText(value);
      setCopied(label);
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(undefined), 1_400);
    } catch (error) {
      reportError(error);
    }
  };
  const rotate = async (): Promise<string | undefined> => {
    rotatingClientKey.current = true;
    try {
      setClientKey(await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
      return undefined;
    } catch (error) {
      setClientKey("");
      setClientKeyVisible(false);
      return errorMessage(error);
    } finally {
      rotatingClientKey.current = false;
    }
  };
  const saveLocalApi = async (config: ListenConfig): Promise<string | undefined> => {
    try {
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
        onCopy={copy}
        onToggleKey={() => setClientKeyVisible((visible) => !visible)}
        onRotate={rotate}
        onSave={saveLocalApi}
        onClose={native.close}
      />
    </NativeDialogHost>
  );
}

function NativeWebUiWindow(): React.JSX.Element {
  const native = useNativeWindow("Web UI Settings");
  if (!native.loaded || native.loadError) {
    return <NativeDialogStatus label="Web UI settings" error={native.loadError} onClose={native.close} />;
  }
  const save = async (config: WebUiConfig): Promise<string | undefined> => {
    try {
      native.setState(await desktopApi.saveWebUi(config));
      return undefined;
    } catch (error) {
      return errorMessage(error);
    }
  };
  return <NativeDialogHost><WebUiSheet state={native.state} onSave={save} onClose={native.close} /></NativeDialogHost>;
}

function NativeUsageProofWindow({ initialRecordId }: { initialRecordId: string }): React.JSX.Element {
  const [recordId, setRecordId] = useState(initialRecordId);
  const initialState = useContext(NativeStateContext);
  const { data: activity, error: recordError } = useQuery({
    queryKey: ["usage-record", recordId], queryFn: () => desktopApi.getUsageRecord(recordId),
    initialData: initialState?.activity.find((item) => item.id === recordId),
  });
  const error = recordError ? errorMessage(recordError) : undefined;
  const native = useNativeWindow("Usage Proof", { contentReady: Boolean(activity), contentError: error });
  useEffect(() => desktopApi.onUsageProofRequest(setRecordId), []);
  if (!activity || error || native.loadError) return <NativeDialogStatus label="usage proof" error={error ?? native.loadError} onClose={native.close} />;
  return <NativeDialogHost ><UsageEvidenceSheet activity={activity} onClose={native.close} /></NativeDialogHost>;
}

export function NativeWindowContent(): React.JSX.Element | null {
  const nativeDialog = query.get("native-dialog");
  const [request, setRequest] = useState<NativeWindowRequest | null>(() => web ? null : ({
    state: initialAppState, repair: query.get("repair") === "1",
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
  const content = nativeWindowContent(request.kind ?? nativeDialog, request);
  return <NativeStateContext.Provider key={generation} value={request.state}>{content}</NativeStateContext.Provider>;
}

function nativeWindowContent(
  kind: string | null,
  request: NativeWindowRequest,
): React.JSX.Element | null {
  switch (kind) {
    case "profiles": return <NativeProfilesWindow repair={request.repair} />;
    case "local-api-example": return <NativeLocalApiExampleWindow />;
    case "notifications": return <NotificationsProvider api={desktopApi}><NativeNotificationsWindow /></NotificationsProvider>;
    case "profile-editor": return <NativeProfilesWindow repair={false} editor profileId={request.profileId} startAfterSave={request.startAfterSave} />;
    case "privacy": return <NativePrivacyWindow />;
    case "local-api": return <NativeLocalApiWindow />;
    case "web-ui": return <NativeWebUiWindow />;
    case "usage-proof": return <NativeUsageProofWindow initialRecordId={request.recordId ?? ""} />;
    default: return null;
  }
}
