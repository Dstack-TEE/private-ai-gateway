import React, { createContext, lazy, memo, Suspense, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useWindowReady } from "./lib/use-window-ready";
import {
  BatteryMedium,
  Bot,
  ExternalLink,
  Ban,
  ChartNoAxesColumn,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  CircleHelp,
  Info,
  Copy,
  Download,
  Eye,
  EyeOff,
  LayoutGrid,
  LoaderCircle,
  LockOpen,
  Network,
  Pencil,
  Plus,
  RefreshCw,
  Settings,
  ShieldCheck,
  ShieldX,
  TriangleAlert,
  Trash2,
  Wifi,
} from "lucide-react";
import { save } from "@tauri-apps/plugin-dialog";
import claudeCodeIcon from "@lobehub/icons-static-svg/icons/claudecode-color.svg";
import codexIcon from "@lobehub/icons-static-svg/icons/codex-color.svg";
import hermesIcon from "@lobehub/icons-static-svg/icons/hermesagent.svg";
import openCodeIcon from "@lobehub/icons-static-svg/icons/opencode.svg";
import openClawIcon from "@lobehub/icons-static-svg/icons/openclaw-color.svg";
import piIcon from "@lobehub/icons-static-svg/icons/pi.svg";
import phalaServiceIcon from "./assets/service-phala.svg";
import redpillServiceIcon from "./assets/service-redpill.png";

import { desktopApi as liveApi, initialGatewayState } from "./desktop-api";
import { brand } from "./generated/brand";
import { mockApi } from "./mock-api";
import { UpdateControl, UpdateChannelControl, UpdateProgressDialog, UpdateProgressMeter, useUpdates } from "./updates";
import type { UpdateProgress } from "../shared/contracts";
import { Button } from "./components/ui/button";
import { ActionItem } from "./components/action-item";
import { UsageChart, type UsageMetric } from "./components/usage-chart";
import { StateLabel } from "./components/state-label";
import { LocalApiExamples } from "./components/local-api-examples";
import { ListenAddress, localAddressKind } from "./components/listen-address";
import { NetworkWarning } from "./components/network-warning";
import { Hint } from "./components/hint";
import { TooltipProvider } from "./components/ui/tooltip";
import { AgentAttention } from "./components/agent-attention";
import { AppearanceProvider, AppearanceControl, useAppearance } from "./components/appearance";
import { NotificationsProvider, NotificationsSheet, useNotifications } from "./components/notifications";
import ohMyPiIcon from "./assets/oh-my-pi.svg";
import { ProfileTransfer, ExportDiagnostics } from "./components/maintenance";
import { installNativeInteractions } from "./lib/native-interactions";
import { DialogCloseProvider, useDialogClose } from "./components/dialog-close";
import { agentName, currency, formatTokens, outcomeOf, usageTokens, type Tone } from "./lib/usage-presentation";
import { usageDateBounds, usageDateLabel, type UsageDateSelection } from "./lib/usage-dates";
import { Field, FieldGroup, FieldLabel, FieldDescription, FieldError, FieldSet, FieldLegend, FieldSeparator } from "./components/ui/field";
import { Badge } from "./components/ui/badge";
import { Alert, AlertDescription } from "./components/ui/alert";
import { SidebarProvider, SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./components/ui/sidebar";
import { Item, ItemActions, ItemContent, ItemTitle, ItemDescription } from "./components/ui/item";
import { Card, CardHeader, CardTitle, CardAction, CardContent } from "./components/ui/card";
import { Separator } from "./components/ui/separator";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./components/ui/collapsible";
import { Input } from "./components/ui/input";
import { InputGroup, InputGroupInput, InputGroupAddon, InputGroupButton } from "./components/ui/input-group";
import { IconButton, SwitchControl } from "./components/controls";
import { Sheet, SheetActions, DismissSheetAction } from "./components/sheet";
import { SettingsSection, SettingsList, SettingsLink, SettingsToggle, FormField } from "./components/settings";
import { ChoiceSelect } from "./components/choice-select";
import { ToggleGroup, ToggleGroupItem } from "./components/ui/toggle-group";
import type {
  AgentStatus,
  CliRegistration,
  ConfidentialProfile,
  ConfidentialProfileInput,
  DesktopApi,
  GatewayState,
  LocalApiConfig,
  LaunchPreferences,
  RequestActivity,
  UsagePage,
  UsageQuery,
  UsageSummary,
  VerificationCheck,
} from "../shared/contracts";

// `?mock=<scenario>` renders the window against canned state for screenshots.
const query = new URLSearchParams(window.location.search);
const UsageDatePicker = lazy(() => import("./components/usage-date-picker").then((module) => ({ default: module.UsageDatePicker })));
const UsageTable = lazy(() => import("./components/usage-table").then((module) => ({ default: module.UsageTable })));
const previewMode = query.has("mock");
const desktopApi: DesktopApi = previewMode ? mockApi(query.get("mock")) : liveApi;

const INITIAL_STATE: GatewayState = {
  status: "stopped",
  configurationVerification: false,
  checks: [],
  activity: [],
  sessionUsage: {
    requests: 0,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
    protected: 0,
    blockedLocally: 0,
    failedProof: 0,
  },
  usageRevision: 0,
  config: { remoteUrl: brand.service.defaultUrl, requireProductionOs: true },
  profiles: [],
  activeProfileId: "",
  localApi: { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 },
  apiKeySaved: false,
};

function unavailableState(error: unknown): GatewayState {
  return {
    ...INITIAL_STATE,
    status: "error",
    backendConnected: false,
    endpointError: "The background service is unavailable.",
    error: errorMessage(error),
  };
}

const CHECK_TITLES: Record<string, string> = {
  "id-1": "Hardware attestation is genuine",
  "id-2": "Attestation is bound to this session",
  "id-3": "Service keys are current",
  "id-4": "Service is built from public source",
  "id-5": "Private key stays inside the enclave",
  "id-6": "Connection uses the attested key",
  "policy-os": "Production OS image",
  "receipt-1": "Receipt signature",
  "receipt-2": "Receipt matches verified service",
  "receipt-3": "Request bytes match receipt",
  "receipt-4": "Response bytes match receipt",
  "receipt-note": "Service request rewrite",
  "upstream-1": "Upstream inference was verified",
  "upstream-2": "Upstream session evidence",
};

const AGENT_ICONS: Record<string, string> = {
  codex: codexIcon,
  "claude-code": claudeCodeIcon,
  opencode: openCodeIcon,
  pi: piIcon,
  "oh-my-pi": ohMyPiIcon,
  hermes: hermesIcon,
  openclaw: openClawIcon,
};

type ServicePreset = "phala" | "redpill" | "custom";
const SERVICE_PRESETS = [
  { id: "phala", name: "Phala", url: "https://inference.phala.com", icon: phalaServiceIcon, keyLabel: "Phala AI API key" },
  { id: "redpill", name: "RedPill", url: "https://tee.redpill.ai", icon: redpillServiceIcon, keyLabel: "RedPill API key" },
] as const;

function servicePreset(url: string): (typeof SERVICE_PRESETS)[number] | undefined {
  const normalized = url.trim().replace(/\/$/, "");
  return SERVICE_PRESETS.find((service) => service.url === normalized);
}

function serviceKeyLabel(url: string): string {
  return servicePreset(url)?.keyLabel ?? "API key";
}

function profileHasCredential(profile: ConfidentialProfile): boolean {
  return profile.credentialSaved ?? Boolean(profile.verifiedAt);
}

function profileIsAvailable(profile: ConfidentialProfile | undefined, state: GatewayState): boolean {
  return Boolean(profile?.verifiedAt && profileHasCredential(profile) && (profile.id !== state.activeProfileId || state.apiKeySaved));
}

function isProtected(state: GatewayState): boolean {
  return state.status === "verified" && !state.configurationVerification && state.apiKeySaved && !state.endpointError;
}

function hasLiveVerification(state: GatewayState): boolean {
  // The runtime admits a session only after sidecar verification and catalog loading.
  return isProtected(state)
    && state.identity?.trustLevel === "hardware_verified";
}

function ProtectionStatus({ state, label }: { state: GatewayState; label: string }): React.JSX.Element {
  const active = isProtected(state);
  const since = active ? state.protectedSince : undefined;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === undefined) return;
    let timer: number | undefined;
    const syncVisibility = () => {
      window.clearInterval(timer);
      if (document.hidden) return;
      setNow(Date.now());
      timer = window.setInterval(() => setNow(Date.now()), 1_000);
    };
    document.addEventListener("visibilitychange", syncVisibility);
    syncVisibility();
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", syncVisibility);
    };
  }, [since]);
  const seconds = since === undefined ? undefined : Math.max(0, Math.floor(now / 1_000) - since);
  const elapsed = seconds === undefined ? undefined : [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60].map((value) => String(value).padStart(2, "0")).join(":");
  return (
    <span className="protection-status">
      {active ? <ShieldCheck size={14} aria-hidden="true" /> : state.reconnecting ? <RefreshCw size={14} aria-hidden="true" /> : <ShieldX size={14} aria-hidden="true" />}
      <span aria-live="polite">{label}</span>
      {elapsed !== undefined && <time className="protection-duration" dateTime={`PT${seconds}S`} aria-label={`Session elapsed ${elapsed}`}>{elapsed}</time>}
    </span>
  );
}

function BrandMark({ className = "", busy = false }: { className?: string; busy?: boolean }): React.JSX.Element {
  const appearance = useAppearance();
  const classes = ["brand-logo", className, busy ? "is-busy" : ""].filter(Boolean).join(" ");
  return (
    <picture className={classes} aria-hidden="true">
      {appearance === "system" && <source media="(prefers-color-scheme: dark)" srcSet={brand.mark.dark} />}
      <img src={appearance === "dark" ? brand.mark.dark : brand.mark.light} alt="" />
    </picture>
  );
}

function ServiceLogo({ url, size = "regular" }: { url: string; size?: "regular" | "large" }): React.JSX.Element {
  const service = servicePreset(url);
  if (!service) {
    return <span className={`service-custom-icon service-logo-${size}`}><Network size={size === "large" ? 16 : 14} /></span>;
  }
  return <span className={`service-logo service-${service.id} service-logo-${size}`}><img src={service.icon} alt="" /></span>;
}

type View = "overview" | "agents" | "usage" | "settings";
type SettingsTarget = "confidential" | "privacy" | "local-api" | "local-api-example" | "notifications";

const VIEWS: { id: View; label: string; icon: typeof LayoutGrid }[] = [
  { id: "overview", label: "Overview", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartNoAxesColumn },
  { id: "settings", label: "Settings", icon: Settings },
];


const TLS_TRACKS = [
  "17 03 03 00 f4   9f3a c1e0 7b42 d5a8 0e6f 2c91 4d17 e8b3 5a0c f9d2 61b7 a3e4 b8c5 0f2e 93d1 7a46 e5b0 1c8d",
  "17 03 03 03 1a   4d17 e8b3 5a0c f9d2 61b7 a3e4 b8c5 0f2e 93d1 7a46 e5b0 1c8d 2e7f a94b 6d03 c1e8 5f27 b6a9",
  "application_data   record_len 244   17 03 03 00 f4   6d03 c1e8 5f27 b6a9 70d2 3b8e c4f1 a90d 1e6c 8b35 e1a7 5c09 f38d 2b64",
  "17 03 03 01 6c   e1a7 5c09 f38d 2b64 d0e7 4a1f 6b0c 8e52 1d9f a7c3 3e08 f5b4 c3d6 4f81 b2a0 7e95 0d1b 9c6e",
  "17 03 03 00 5e   b2a0 7e95 0d1b 9c6e 18e4 a0f7 5b3c d29a 6c04 e7f1 8d2b f6c0 3a17 e94d b5c8 02a6 5f7e 1b93",
  "application_data   record_len 794   17 03 03 03 1a   b5c8 02a6 5f7e 1b93 c80a d4e2 76b1 3d0c 9e21 4fb7 a6d5 0c83 e2f9 71b4",
  "17 03 03 02 48   9e21 4fb7 a6d5 0c83 e2f9 71b4 5d0e 8ac6 3f92 b7e0 6a1d c95f 2d38 f04b 81c7 e6a2 5b9d 1f74",
  "17 03 03 00 91   81c7 e6a2 5b9d 1f74 c0e3 a8d6 4e27 9b1c d5f0 3c68 7e4a f2c4 0b9e 6d17 a3e8 5c02 e9b1 4d7f",
  "17 03 03 01 d0   a3e8 5c02 e9b1 4d7f 8a36 1e0c b5d9 7f23 c6a4 0e81 d3b7 2a5c 9f6e 4b10 c7d2 3e5a 90f4 1b6c",
  "application_data   record_len 152   17 03 03 00 98   c7d2 3e5a 90f4 1b6c 8d07 e2a9 5f31 b48e 7a0d 2c95 f6e3 41b8 d9c0 3f5e",
  "17 03 03 00 3c   7a0d 2c95 f6e3 41b8 d9c0 3f5e 8a2b 6e17 c4d8 0b93 5a6f e1d2 7c04 93ab 5e8f 21c6 d0a3 7b19",
];

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
  const [state, setState] = useState<GatewayState>(initialState ?? INITIAL_STATE);
  const [loaded, setLoaded] = useState(Boolean(initialState));
  const [loadError, setLoadError] = useState<string>();
  const [closed, setClosed] = useState(false);
  useWindowReady(loaded && (contentReady || Boolean(loadError)) && !closed, desktopApi.nativeDialogReady, setLoadError);

  useEffect(() => {
    document.title = `${title} - ${brand.productName}`;
    const root = document.documentElement;
    root.classList.add("is-native-dialog");
    root.style.setProperty("--accent-light", brand.theme.accentLight);
    root.style.setProperty("--accent-dark", brand.theme.accentDark);
    let active = true;
    let receivedState = false;
    const unsubscribe = desktopApi.onStateChange((nextState) => {
      receivedState = true;
      if (active) setState(nextState);
    });
    void desktopApi.getState().then(
      (nextState) => {
        if (!active) return;
        if (!receivedState) setState(nextState);
        setLoaded(true);
      },
      (error: unknown) => {
        if (!active) return;
        setLoadError(errorMessage(error));
        setLoaded(true);
      },
    );
    return () => {
      active = false;
      unsubscribe();
      root.classList.remove("is-native-dialog");
    };
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
  return <main className="native-dialog-host p-6 flex flex-col gap-4" aria-labelledby="update-title">
    <h2 id="update-title" className="text-lg font-semibold">{progress?.error ? "Update failed" : "Installing update"}</h2>
    <p className="min-h-0 overflow-auto break-words text-sm text-muted-foreground" role={progress?.error ? "alert" : undefined}>{progress?.error ?? "The app will restart when installation completes."}</p>
    {!progress?.error && <UpdateProgressMeter progress={progress} />}
    {native.loadError && <p role="alert" className="text-sm text-destructive">{native.loadError}</p>}
    {progress?.error && <div className="mt-auto flex justify-end"><Button variant="outline" onClick={native.close}>Done</Button></div>}
  </main>;
}

function NativeDialogStatus({ label, error, onClose }: { label: string; error?: string; onClose(): void }): React.JSX.Element | null {
  useDialogClose(onClose, Boolean(error), Boolean(error));
  // The native window remains hidden until content or an actionable error is ready.
  if (!error) return null;
  return (
    <main className="native-dialog-host native-dialog-loading" aria-label={label}>
      <TriangleAlert aria-hidden="true" />
      <span className="min-h-0 overflow-auto break-words" role="alert">{error}</span>
      <Button variant="outline" onClick={onClose}>Done</Button>
    </main>
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

  if (native.closed) return <main className="native-dialog-host" aria-label="Profiles closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="profiles" error={native.loadError} onClose={native.close} />;
  const busy = native.state.status === "verifying";
  const running = !native.state.configurationVerification && (native.state.status === "verified" || native.state.status === "blocked");
  const editingProfileId = profileId;
  const editingProfile = native.state.profiles.find((profile) => profile.id === editingProfileId);
  if (editor && editingProfileId && !editingProfile) return <NativeDialogStatus label="profile" error="This profile is no longer available." onClose={native.close} />;
  if (editor) return <main className="native-dialog-host"><ProfileEditorSheet
    state={native.state} busy={busy} running={running}
    profile={editingProfile}
    onVerify={(profile, key) => run(async () => {
      const saved = await desktopApi.verifyConfiguration(profile, native.state.config.requireProductionOs, key);
      return startAfterSave ? desktopApi.start(saved.config) : saved;
    })}
    onDelete={(profileId) => run(() => desktopApi.deleteProfile(profileId))}
    onComplete={native.close} onDeleted={native.close} onClose={native.close}
  /></main>;
  return (
    <main className="native-dialog-host">
      {actionError && <div className="sr-only" role="alert">{actionError}</div>}
      <ProfilesSheet
        key={repairRequest}
        state={native.state}
        busy={busy}
        running={running}
        initialEditorProfileId={repairRequest ? native.state.activeProfileId || undefined : undefined}
        onVerify={(profile, key) => run(() => desktopApi.verifyConfiguration(profile, native.state.config.requireProductionOs, key))}
        onActivate={(profileId) => run(() => desktopApi.activateProfile(profileId))}
        onDelete={(profileId) => run(() => desktopApi.deleteProfile(profileId))}
        onClose={native.close}
      />
    </main>
  );
}

function NativeNotificationsWindow(): React.JSX.Element {
  const { data, error } = useNotifications();
  const native = useNativeGatewayWindow("Notifications", Boolean(data || error));
  if (native.closed) return <main aria-label="Notifications closed" />;
  return <main className="native-dialog-host"><NotificationsSheet onClose={native.close} /></main>;
}

function NativeLocalApiExampleWindow(): React.JSX.Element {
  const [exampleReady, setExampleReady] = useState(false);
  const native = useNativeGatewayWindow("Local API examples", exampleReady);
  if (native.closed) return <main className="native-dialog-host" aria-label="Local API examples closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="Local API examples" error={native.loadError} onClose={native.close} />;
  return <main className="native-dialog-host"><LocalApiExamples
    api={desktopApi}
    onReady={() => setExampleReady(true)}
    endpoint={native.state.proxyUrl ?? localEndpoint(native.state.localApi)}
    models={native.state.catalog?.models ?? []}
    onCopy={(value) => desktopApi.copyText(value)} onClose={native.close}
  /></main>;
}

function NativePrivacyWindow(): React.JSX.Element {
  const native = useNativeGatewayWindow("Privacy Verification");
  if (native.closed) return <main className="native-dialog-host" aria-label="Privacy verification closed" />;
  if (!native.loaded || native.loadError) return <NativeDialogStatus label="privacy verification" error={native.loadError} onClose={native.close} />;
  return (
    <main className="native-dialog-host">
      <PrivacyVerificationSheet state={native.state} onClose={native.close} />
    </main>
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

  if (native.closed) return <main className="native-dialog-host" aria-label="Local API settings closed" />;
  if (!native.loaded || !keyLoaded || native.loadError || keyError) {
    return <NativeDialogStatus label="Local API settings" error={native.loadError ?? keyError} onClose={native.close} />;
  }
  const busy = native.state.status === "verifying";
  const running = !native.state.configurationVerification && (native.state.status === "verified" || native.state.status === "blocked");
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
    <main className="native-dialog-host">
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
    </main>
  );
}

function NativeUsageProofWindow({ initialRecordId }: { initialRecordId: string }): React.JSX.Element {
  const [recordId, setRecordId] = useState(initialRecordId);
  const initialState = useContext(NativeStateContext);
  const [activity, setActivity] = useState<RequestActivity | undefined>(() => initialState?.activity.find((item) => item.id === initialRecordId));
  const [error, setError] = useState<string>();
  const native = useNativeGatewayWindow("Usage Proof", Boolean(activity || error));
  useEffect(() => desktopApi.onUsageProofRequest(setRecordId), []);
  useEffect(() => {
    let active = true;
    setActivity((current) => current?.id === recordId ? current : undefined);
    setError(undefined);
    void desktopApi.getUsageRecord(recordId).then(
      (record) => active && setActivity(record),
      (loadError: unknown) => active && setError(errorMessage(loadError)),
    );
    return () => { active = false; };
  }, [recordId]);
  if (native.closed) return <main className="native-dialog-host" aria-label="Usage proof closed" />;
  if (!activity || error || native.loadError) return <NativeDialogStatus label="usage proof" error={error ?? native.loadError} onClose={native.close} />;
  return <main className="native-dialog-host"><UsageEvidenceSheet activity={activity} onClose={native.close} /></main>;
}

function App({ initialView = "overview" }: { initialView?: View }): React.JSX.Element {
  const updates = useUpdates(desktopApi, !previewMode);
  const [view, setView] = useState<View>(initialView);
  const [settingsTarget, setSettingsTarget] = useState<SettingsTarget>();
  const [profileEditorId, setProfileEditorId] = useState<string>();
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [stateLoaded, setStateLoaded] = useState(false);
  const [allowDevelopmentOs, setAllowDevelopmentOs] = useState(false);
  const [launchPreferences, setLaunchPreferences] = useState<LaunchPreferences>();
  const [savingPreference, setSavingPreference] = useState(false);
  const [connectingBackend, setConnectingBackend] = useState(false);
  const [actionError, setActionError] = useState<string>();
  useWindowReady(stateLoaded, desktopApi.mainWindowReady, setActionError);
  const [clientKeyError, setClientKeyError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [pendingAgentChanges, setPendingAgentChanges] = useState<Record<string, boolean>>({});
  const agentOperations = useRef(new Set<string>());
  const agentIntents = useRef(new Map<string, boolean>());
  const agentScanFlight = useRef<Promise<AgentStatus[] | undefined> | undefined>(undefined);
  const agentScanQueued = useRef(false);
  const [applying, setApplying] = useState(false);
  const [selectedUsage, setSelectedUsage] = useState<RequestActivity>();
  const [notice, setNotice] = useState<{ id: number; text: string } | undefined>(() => initialView === "settings" ? { id: Date.now(), text: "Settings reset" } : undefined);
  const [previewTrayOpen, setPreviewTrayOpen] = useState(false);
  const copyTimer = useRef<number | undefined>(undefined);
  const [startAfterSetup, setStartAfterSetup] = useState(false);
  const agentScan = useRef(0);
  const busy = state.status === "verifying";
  const running = !state.configurationVerification && (state.status === "verified" || state.status === "blocked");
  const verified = !state.configurationVerification && state.status === "verified";
  const endpointDown = Boolean(state.endpointError);
  const models = state.catalog?.models ?? [];

  useEffect(() => {
    let active = true;
    void desktopApi.getLaunchPreferences().then(
      (value) => { if (active) setLaunchPreferences(value); },
      (error: unknown) => { if (active) setActionError(errorMessage(error)); },
    );
    const unsubscribe = desktopApi.onLaunchPreferencesChange(setLaunchPreferences);
    return () => { active = false; unsubscribe(); };
  }, []);

  const saveLaunchPreference = async (name: keyof LaunchPreferences, enabled: boolean) => {
    setSavingPreference(true);
    setActionError(undefined);
    try { setLaunchPreferences(await desktopApi.setLaunchPreference(name, enabled)); }
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
    const root = document.documentElement.style;
    root.setProperty("--accent-light", brand.theme.accentLight);
    root.setProperty("--accent-dark", brand.theme.accentDark);
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
    const unsubscribe = desktopApi.onStateChange((nextState) => {
      if (active) {
        setState(nextState);
        setStateLoaded(true);
      }
    });
    const unsubscribeNavigate = desktopApi.onNavigate((section) => {
      if (active) {
        setSettingsTarget(undefined);
        setView(section);
        window.requestAnimationFrame(() => document.getElementById(`page-title-${section}`)?.focus());
      }
    });
    void desktopApi.getState().then(
      (nextState) => {
        if (!active) return;
        setState(nextState);
        setStateLoaded(true);
      },
      (error: unknown) => {
        if (!active) return;
        setState(unavailableState(error));
        setStateLoaded(true);
        setActionError(errorMessage(error));
      },
    );
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
      unsubscribe();
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

  const loadAgents = useCallback((fresh = false) => {
    if (agentScanFlight.current) {
      if (fresh) agentScanQueued.current = true;
      return agentScanFlight.current;
    }
    const request = (async () => {
      let next: AgentStatus[] | undefined;
      do {
        agentScanQueued.current = false;
        const scan = ++agentScan.current;
        try {
          next = await desktopApi.listAgents();
          if (scan === agentScan.current) setAgents(next);
        } catch (error) {
          next = undefined;
          if (scan === agentScan.current) setActionError(errorMessage(error));
        }
      } while (agentScanQueued.current);
      return next;
    })().finally(() => { agentScanFlight.current = undefined; });
    agentScanFlight.current = request;
    return request;
  }, []);

  useEffect(() => {
    const refresh = () => { agentScan.current += 1; void loadAgents(true); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [loadAgents]);
  useEffect(() => { if (view === "agents") void loadAgents(true); }, [view, loadAgents]);

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

  // Agent status depends on the verified catalog, so reload with the session.
  const catalogRevision = state.catalog?.revision;
  useEffect(() => {
    let active = true;
    let timer: number | undefined;
    const refresh = async (fresh = false) => {
      await loadAgents(fresh);
      if (active) timer = window.setTimeout(() => void refresh(), 5_000);
    };
    agentScan.current += 1;
    void refresh(true);
    return () => { active = false; window.clearTimeout(timer); };
  }, [loadAgents, catalogRevision, verified]);
  useEffect(() => desktopApi.onAgentsChange(() => {
    agentScan.current += 1;
    void loadAgents(true);
  }), [loadAgents]);

  const run = async (action: () => Promise<GatewayState | void>) => {
    setActionError(undefined);
    try {
      const next = await action();
      if (next) {
        setState(next);
      }
    } catch (error) {
      setActionError(errorMessage(error));
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

  const verifyConfiguration = async (profile: ConfidentialProfileInput, key?: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      const saved = await desktopApi.verifyConfiguration(profile, !allowDevelopmentOs, key);
      setState(startAfterSetup ? await desktopApi.start(saved.config) : saved);
      setStartAfterSetup(false);
      setNotice({ id: Date.now(), text: `${profile.name.trim()} verified and saved` });
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const activateProfile = async (profileId: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      setState(await desktopApi.activateProfile(profileId));
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const deleteProfile = async (profileId: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      setState(await desktopApi.deleteProfile(profileId));
      setNotice({ id: Date.now(), text: "AI service profile deleted" });
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const rotateClientKey = async () => {
    setActionError(undefined);
    try {
      setClientKey(await desktopApi.rotateClientKey());
      setClientKeyVisible(true);
      setNotice({ id: Date.now(), text: "Client key replaced" });
    } catch (error) {
      setClientKey("");
      setClientKeyVisible(false);
      setActionError(errorMessage(error));
    }
  };

  const saveLocalApi = async (config: LocalApiConfig): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      setState(await desktopApi.saveLocalApiConfig(config));
      setNotice({ id: Date.now(), text: "Local API settings saved" });
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const copy = async (label: string, value: string) => {
    await run(async () => {
      await desktopApi.copyText(value);
      setCopied(label);
      setNotice({ id: Date.now(), text: `${label} copied` });
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(
        () => setCopied((current) => (current === label ? undefined : current)),
        1_400,
      );
    });
  };

  const applyAgent = async (agent: AgentStatus, connect: boolean) => {
    agentIntents.current.set(agent.id, connect);
    setPendingAgentChanges((current) => ({ ...current, [agent.id]: connect }));
    if (agentOperations.current.has(agent.id)) return;
    agentOperations.current.add(agent.id);
    agentScan.current += 1;
    setActionError(undefined);
    try {
      let changed = agent;
      // Serialize writes per agent and retain the user's latest intent.
      while (agentIntents.current.has(agent.id)) {
        const target = agentIntents.current.get(agent.id);
        if (target === undefined) break;
        if (changed.recorded !== target || (target && !changed.authorized && changed.attention)) {
          const options = target && agent.id === "codex" && !agent.recorded ? { defaultModel: models[0]?.id } : {};
          const preview = await desktopApi.previewAgent(agent.id, target, options);
          const status = await desktopApi.applyAgent(agent.id, target, preview.revision, options);
          changed = status;
          agentScan.current += 1;
          setAgents((current) => current.map((entry) => entry.id === status.id ? status : entry));
        }
        if (agentIntents.current.get(agent.id) === target) {
          agentIntents.current.delete(agent.id);
        }
      }
      setNotice({ id: Date.now(), text: `${displayAgentName(agent)} ${changed.recorded ? "connected" : "disconnected"}` });
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      agentIntents.current.delete(agent.id);
      agentOperations.current.delete(agent.id);
      setPendingAgentChanges((current) => { const next = { ...current }; delete next[agent.id]; return next; });
      void loadAgents(true);
    }
  };

  const resetSettings = async () => {
    setActionError(undefined);
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: "Reset settings?",
        message: "Stop protection, disconnect all agents and restore their configurations, and reset appearance, notifications, startup preferences, development OS policy, update channel, Local API settings, and window size. Profiles, credentials, the local API key, and usage history are kept. This does not change system notification permission or uninstall the pap command.",
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
      setNotice({ id: Date.now(), text: "Settings reset" });
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
    <main className="app-shell">
      <Sidebar view={view} previewControls={previewMode} updateAvailable={Boolean(updates.info?.version)} updateBusy={Boolean(updates.busy)} onInstallUpdate={() => void updates.install()} onChange={changeView} />
      <section className="workspace">
        <PageHeader
          view={view}
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
          developmentMode={allowDevelopmentOs}
          onToggle={toggleGateway}
        />
        <div className="content" id={`page-${view}`} key={view}>
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
            onNotice={(text) => setNotice({ id: Date.now(), text })}
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
          onVerify={verifyConfiguration}
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
    return <div className="native-host">{windowContent}</div>;
  }

  return (
    <div className="desktop-preview">
      <MacMenuBar protected={isProtected(state)} trayOpen={previewTrayOpen} onTray={() => setPreviewTrayOpen((open) => !open)} />
      <div className="desktop-window">{windowContent}</div>
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
            setNotice({ id: Date.now(), text: "Quit is available in the installed macOS app" });
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

function Sidebar({
  view,
  previewControls,
  onChange,
  updateAvailable,
  updateBusy,
  onInstallUpdate,
}: {
  view: View;
  previewControls: boolean;
  updateAvailable: boolean;
  updateBusy: boolean;
  onInstallUpdate(): void;
  onChange(view: View, focusHeading?: boolean): void;
}): React.JSX.Element {
  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    const index = VIEWS.findIndex((entry) => entry.id === view);
    const step = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
    if (step === 0) {
      return;
    }
    event.preventDefault();
    const next = VIEWS[(index + step + VIEWS.length) % VIEWS.length]?.id ?? view;
    onChange(next, false);
    (event.currentTarget.querySelector(`#nav-${next}`) as HTMLElement | null)?.focus();
  };
  return (
    <aside className={previewMode || /Macintosh|Mac OS X/.test(navigator.userAgent) ? "sidebar" : "sidebar sidebar-standard"}>
      <div className="sidebar-drag" data-tauri-drag-region>
        {previewControls && (
          <span className="traffic-lights" aria-hidden="true">
            <span className="traffic-close" />
            <span className="traffic-minimize" />
            <span className="traffic-zoom" />
          </span>
        )}
      </div>
      <div className="sidebar-brand" data-tauri-drag-region>
        <BrandMark className="brand-mark" />
        <span className="sidebar-brand-copy"><span>{brand.productName}</span><small>by dstack TEE</small></span>
      </div>
      <SidebarProvider keyboardShortcut={false} className="min-h-0 flex-col">
      <nav className="w-full" aria-label="Main navigation" onKeyDown={onKeyDown}>
        <SidebarMenu>
        {VIEWS.map((entry) => {
          const Icon = entry.icon;
          return (
            <SidebarMenuItem key={entry.id}><SidebarMenuButton
              size="default"
              isActive={view === entry.id}
              id={`nav-${entry.id}`}
              aria-label={entry.label}
              aria-current={view === entry.id ? "page" : undefined}
              tabIndex={view === entry.id ? 0 : -1}
              onClick={() => onChange(entry.id, true)}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{entry.label}</span>
            </SidebarMenuButton></SidebarMenuItem>
          );
        })}
        </SidebarMenu>
      </nav>
      </SidebarProvider>
      {updateAvailable && <div className="mt-auto pt-4">
        <Badge variant="outline" className="h-8 w-full gap-2 text-sm hover:bg-muted [&>svg]:size-4!" render={<button type="button" disabled={updateBusy} />} aria-label="Update available" onClick={onInstallUpdate}>
          <Download aria-hidden="true" /><span className="max-[620px]:hidden">Update available</span>
        </Badge>
      </div>}
    </aside>
  );
}

function MacMenuBar({ protected: isProtected, trayOpen, onTray }: { protected: boolean; trayOpen: boolean; onTray(): void }): React.JSX.Element {
  const date = new Intl.DateTimeFormat("en-US", {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date());
  return (
    <div className="mac-menu-bar">
      <div className="mac-menu-left" aria-hidden="true">
        <span className="mac-apple" aria-hidden="true">◆</span>
        <strong>{brand.productName}</strong>
        <span>File</span><span>Edit</span><span>View</span><span>Window</span><span>Help</span>
      </div>
      <div className="mac-menu-right">
        <Button variant="ghost" className={`tray-trigger${trayOpen ? " is-open" : ""}`} aria-label="Private AI Proxy menu" aria-expanded={trayOpen} onClick={onTray}>
          <span className={`tray-template-icon${isProtected ? " is-protected" : ""}`} aria-hidden="true" />
        </Button>
        <Wifi size={15} strokeWidth={1.8} aria-hidden="true" />
        <BatteryMedium size={17} strokeWidth={1.8} aria-hidden="true" />
        <time aria-hidden="true">{date}</time>
      </div>
    </div>
  );
}

function PreviewTrayMenu({
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  openAtLogin,
  onProtection,
  onOpen,
  onSettings,
  onOpenAtLogin,
  onQuit,
  onStopAllQuit,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  openAtLogin: boolean;
  onProtection(): void;
  onOpen(): void;
  onSettings(): void;
  onOpenAtLogin(): void;
  onQuit(): void;
  onStopAllQuit(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const verifying = state.status === "verifying" && !state.configurationVerification;
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const action = verifying ? "Cancel verification" : running ? "Stop protection"
    : profileIsAvailable(activeProfile, state) ? "Start protection" : "Set Up Profile…";
  return (
    <div className="preview-tray" role="menu" aria-label="Private AI Proxy">
      <div className="preview-tray-heading">
        <BrandMark />
        <span><strong>{brand.productName}</strong><small>{serviceHost(state.remoteUrl ?? state.config.remoteUrl)}</small></span>
      </div>
      <div className="preview-tray-status" role="status">{verdict.title}{developmentMode ? " (Dev mode)" : ""}</div>
      <Button variant="ghost" className="preview-tray-item" role="menuitem" disabled={(busy && !verifying) || (endpointDown && !running && !verifying)} onClick={onProtection}>{action}</Button>
      <div className="preview-tray-separator" />
      <Button variant="ghost" className="preview-tray-item" role="menuitem" onClick={onOpen}>Open {brand.productName}</Button>
      <Button variant="ghost" className="preview-tray-item" role="menuitem" onClick={onSettings}>Settings…</Button>
      <div className="preview-tray-separator" />
      <Button variant="ghost" className="preview-tray-item" role="menuitemcheckbox" aria-checked={openAtLogin} onClick={onOpenAtLogin}>
        <span className="preview-tray-check" aria-hidden="true">{openAtLogin ? "✓" : ""}</span>
        Open at Login
      </Button>
      <Button variant="ghost" className="preview-tray-item" role="menuitem" onClick={onQuit}>Quit {brand.productName}</Button>
      <Button variant="ghost" className="preview-tray-item" role="menuitem" onClick={onStopAllQuit}>Stop All and Quit…</Button>
    </div>
  );
}

function PageHeader({
  view,
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  onToggle,
}: {
  view: View;
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const title = VIEWS.find((entry) => entry.id === view)?.label ?? "";
  const verdict = presentation(state);
  return (
    <header className="page-header" data-tauri-drag-region>
      <h1 id={`page-title-${view}`} tabIndex={-1}>{title}</h1>
      {view !== "overview" && (
        <div className="page-protection">
          {developmentMode && <span className="state state-warning">Dev mode</span>}
          <span className={`page-switch-copy state-${verdict.tone}`}>
            <strong><ProtectionStatus state={state} label={verdict.title} /></strong>
          </span>
          <ProtectedControl
            state={state}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            developmentMode={developmentMode}
            compact
            iconOnly
            onToggle={onToggle}
          />
        </div>
      )}
    </header>
  );
}

function Overview({
  pendingAgentChanges,
  state,
  agents,
  busy,
  running,
  endpointDown,
  developmentMode,
  problem,
  locked,
  clientKey,
  clientKeyVisible,
  copied,
  onToggle,
  onSettings,
  onPrivacy,
  onLocalSettings,
  onLocalExamples,
  onAgents,
  onUsage,
  onCopy,
  onToggleClientKey,
  onSelect,
  onInspect,
}: {
  pendingAgentChanges: Record<string, boolean>;
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  problem?: string;
  locked: boolean;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  onToggle(): void;
  onSettings(): void;
  onPrivacy(): void;
  onLocalSettings(): void;
  onLocalExamples(): void;
  onAgents(): void;
  onUsage(): void;
  onCopy(label: string, value: string): Promise<void>;
  onToggleClientKey(): void;
  onSelect(agent: AgentStatus, connect: boolean): void;
  onInspect(activity: RequestActivity): void;
}): React.JSX.Element {
  const protectedNow = isProtected(state);
  const localAvailable = isProtected(state) && Boolean(state.proxyUrl) && !state.endpointError;
  const recent = protectedNow || state.sessionActive || state.reconnecting ? state.activity.slice(0, 4) : [];
  return (
    <div className="overview-page">
      <div className="overview-top">
      <StatusSurface
        state={state}
        agents={agents}
        busy={busy}
        running={running}
        endpointDown={endpointDown}
        developmentMode={developmentMode}
        onToggle={onToggle}
        onSettings={onSettings}
        onPrivacy={onPrivacy}
      />
      <SessionSummary summary={state.sessionUsage} active={protectedNow || Boolean(state.sessionActive || state.reconnecting)} />
      </div>
      {problem && (
        <p className="banner overview-banner" role="alert">
          <TriangleAlert size={15} aria-hidden="true" /> {problem}
        </p>
      )}
      <div className="overview-grid">
        <OverviewModule title="Local API" titleAdornment={<Hint content="Local API examples"><Badge variant="ghost" className="size-6 p-0 [&>svg]:size-4!" render={<button type="button" />} aria-label="Local API examples" aria-haspopup="dialog" onClick={onLocalExamples}><CircleHelp aria-hidden="true" /></Badge></Hint>} status={<StateLabel tone={localAvailable ? "success" : "neutral"} text={localAvailable ? "Available" : "Unavailable"} />}>
          <LocalApiPanel
            proxyUrl={state.proxyUrl}
            endpointError={state.endpointError}
            clientKey={clientKey}
            clientKeyVisible={clientKeyVisible}
            copied={copied}
            onCopy={onCopy}
            onSettings={onLocalSettings}
            onToggleKey={onToggleClientKey}
          />
        </OverviewModule>
        <OverviewModule title="Agents" action="View all" onAction={onAgents}>
          <div className="preview-list overview-agent-list">
            {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
            {sortAgents(agents.filter((agent) => agent.installed)).slice(0, 4).map((agent) => (
              <AgentRow
                pendingConnection={pendingAgentChanges[agent.id]}
                key={agent.id}
                agent={agent}
                compact
                disabled={locked}
                onSelect={(connect) => onSelect(agent, connect)}
              />
            ))}
          </div>
        </OverviewModule>
        <OverviewModule
          title="Recent usage"
          action="View all"
          onAction={onUsage}
        >
          <div className="preview-list">
            {recent.length === 0 && (
              <EmptyState text={running ? "No requests in this session yet." : "Start protection to begin a new session."} />
            )}
            {recent.map((item) => (
              <React.Fragment key={item.id}><UsageRow activity={item} onOpen={() => onInspect(item)} /><Separator className="last:hidden" /></React.Fragment>
            ))}
          </div>
        </OverviewModule>
      </div>
    </div>
  );
}

function StatusSurface({
  state,
  agents,
  busy,
  running,
  endpointDown,
  developmentMode,
  onToggle,
  onSettings,
  onPrivacy,
}: {
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  onToggle(): void;
  onSettings(): void;
  onPrivacy(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const protectedNow = isProtected(state);
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <Card size="sm" role="region" className={`status-surface status-compact status-${state.status} ${protectedNow ? developmentMode ? "status-ready ring-warning dark:ring-warning" : "status-ready ring-primary dark:ring-primary" : ""} ${developmentMode ? "is-development" : ""}`} aria-label="Protection status">
      <TrackLayer />
      <CardContent className="status-compact-content">
        <div className={`status-heading state-${verdict.tone}`}>
          <ProtectionStatus state={state} label={verdict.title} />
        </div>
        <div className="status-profile-actions">
        <Button id="overview-profile" variant="outline" size="sm" className="status-profile" aria-label={activeProfile ? `Profiles: ${activeProfile.name}` : "Set up profile"} aria-haspopup="dialog" onClick={onSettings}>
          {activeProfile ? <ServiceLogo url={activeProfile.remoteUrl} /> : <Plus aria-hidden="true" />}
          <span>{activeProfile?.name ?? "Set up"}</span>
          {activeProfile && <ChevronDown aria-hidden="true" />}
        </Button>
        <IconButton size="icon-sm" label="Privacy verification" aria-haspopup="dialog" onClick={onPrivacy}><Info aria-hidden="true" /></IconButton>
        </div>
        <ProtectedControl state={state} busy={busy} running={running} endpointDown={endpointDown} developmentMode={developmentMode} onToggle={onToggle} iconOnly />
      </CardContent>
    </Card>
  );
}

const TrackLayer = memo(function TrackLayer(): React.JSX.Element {
  return (
    <div className="track-layer tracks-right" aria-hidden="true">
      {TLS_TRACKS.map((line, index) => <TrackRow key={line} text={line} reverse={index % 2 === 1} />)}
    </div>
  );
});

function TrackRow({ text, reverse }: { text: string; reverse: boolean }): React.JSX.Element {
  return (
    <div className={`track-row ${reverse ? "track-reverse" : ""}`}>
      <div className="track-strip">
        <span className="track-copy">{text}</span><span className="track-copy">{text}</span>
      </div>
    </div>
  );
}

function ProtectedControl({
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  compact = false,
  iconOnly = false,
  onToggle,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  compact?: boolean;
  iconOnly?: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const protectionStarting = busy && !state.configurationVerification;
  const checked = running || protectionStarting || Boolean(state.reconnecting);
  const label = busy
    ? state.configurationVerification ? "Verifying configuration" : "Cancel protection start"
    : state.reconnecting ? "Cancel reconnection" : running ? "Stop protection" : "Start protection";
  return (
    <div className={`protected-control ${compact ? "is-compact" : ""} ${iconOnly && !compact ? "is-icon-only" : ""}`}>
      {!iconOnly && <span>Protected</span>}
      {developmentMode && !compact && <span className="dev-mode-label">Dev mode</span>}
      <SwitchControl
        size="default"
        checked={checked}
        label={label}
        disabled={(busy && state.configurationVerification) || (endpointDown && !checked)}
        developmentMode={developmentMode}
        onToggle={onToggle}
      />
    </div>
  );
}

function OverviewModule({
  title,
  titleAdornment,
  status,
  action,
  onAction,
  children,
}: React.PropsWithChildren<{
  title: string;
  titleAdornment?: React.ReactNode;
  status?: React.ReactNode;
  action?: string;
  onAction?(): void;
}>): React.JSX.Element {
  return (
    <Card size="sm" className="overview-module">
      <CardHeader className="items-center">
        <CardTitle className="overview-module-title"><h2 className="text-base font-medium">{title}</h2>{titleAdornment}{status}</CardTitle>
        {action && onAction && <CardAction><Button variant="outline" size="sm" onClick={onAction}>{action}</Button></CardAction>}
      </CardHeader>
      <CardContent className="module min-h-0 flex-1">{children}</CardContent>
    </Card>
  );
}

function LocalApiPanel({
  proxyUrl,
  endpointError,
  clientKey,
  clientKeyVisible,
  copied,
  onCopy,
  onSettings,
  onToggleKey,
}: {
  proxyUrl?: string;
  endpointError?: string;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  onCopy(label: string, value: string): Promise<void>;
  onSettings(): void;
  onToggleKey(): void;
}): React.JSX.Element {
  const endpointLabel = "Local endpoint";
  const keyLabel = "Client key";
  return (
    <div className="copy-rows">
      <Item variant="muted" size="xs" className="copy-row overflow-hidden">
        <Button variant="ghost"
          className="copy-surface h-full w-full rounded-none"
          disabled={!proxyUrl}
          aria-label={`${endpointLabel}: ${proxyUrl ?? "Unavailable"}. Copy`}
          onClick={() => proxyUrl && void onCopy(endpointLabel, proxyUrl)}
        >
          <span className="row-title-line">
            <span className="row-title">Endpoint</span>
          </span>
          <code className="row-note">{proxyUrl ?? "Unavailable"}</code>
          <span className={`copy-feedback ${copied === endpointLabel ? "is-copied" : ""}`}>{copied === endpointLabel ? "Copied" : "Copy"}</span>
        </Button>
        <IconButton className="row-action" label="Local API settings" onClick={onSettings}><Settings size={16} /></IconButton>
      </Item>
      <Item variant="muted" size="xs" className="copy-row overflow-hidden">
        <Button variant="ghost" className="copy-surface h-full w-full rounded-none" disabled={!clientKey} aria-label={`${keyLabel}: ${clientKeyVisible ? clientKey : "hidden"}. Copy`} onClick={() => clientKey && void onCopy(keyLabel, clientKey)}>
          <span className="row-title-line">
            <span className="row-title">Client key</span>
          </span>
          <code className="row-note">{clientKey ? clientKeyVisible ? clientKey : maskClientKey(clientKey) : "Unavailable"}</code>
          <span className={`copy-feedback ${copied === keyLabel ? "is-copied" : ""}`}>{copied === keyLabel ? "Copied" : "Copy"}</span>
        </Button>
        <IconButton className="row-action" label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
      </Item>
      {endpointError && <p className="inline-error">{endpointError}</p>}
    </div>
  );
}

function SessionSummary({ summary, active }: { summary: UsageSummary; active: boolean }): React.JSX.Element {
  const forwarded = Math.max(0, summary.requests - summary.blockedLocally);
  const totalTokens = summary.inputTokens + summary.outputTokens;
  const protectedRate = forwarded ? Math.round((summary.protected / forwarded) * 100) : 0;
  return (
    <section className="session-overview" aria-labelledby="session-usage-heading">
      <h2 className="sr-only" id="session-usage-heading">Current session</h2>
    <div className="session-summary" role="group" aria-label="Usage in this session">
      {[
        ["Requests", active ? summary.requests.toLocaleString() : "—"],
        ["Tokens", active ? formatTokens(totalTokens) : "—"],
        ["Estimated cost", active ? currency(summary.costUsd) : "—"],
        ["Verified answers", active && forwarded ? `${protectedRate}%` : "—"],
      ].map(([label, value]) => <Card key={label} size="sm"><CardContent className="grid gap-2"><span className="text-xs text-muted-foreground">{label}</span><strong className="text-xl font-semibold tabular-nums">{value}</strong></CardContent></Card>)}
    </div>
    </section>
  );
}

function UsageRow({ activity, onOpen }: { activity: RequestActivity; onOpen(): void }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const tokens = usageTokens(activity);
  const timestamp = new Date(activity.at * 1_000);
  return (
    <ActionItem size="xs" className="usage-row" onClick={onOpen} aria-label={`${agentName(activity.agent)}, ${outcome.label}, ${activity.model ?? activity.path}. View proof`}>
      <span className="row-main">
        <span className="row-title">{agentName(activity.agent)}</span>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        <code className="row-note">{activity.model ?? activity.path}</code>
      </span>
      <span className="usage-amount"><strong>{tokens === undefined ? "—" : formatTokens(tokens)}</strong><small>tokens</small></span>
      <span className="usage-amount usage-cost"><strong>{activity.costUsd === undefined ? "—" : currency(activity.costUsd)}</strong><small>cost</small></span>
      <time className="row-side" dateTime={timestamp.toISOString()}><span>{timestamp.toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span><span>{formatTimestamp(timestamp.getTime())}</span></time>
    </ActionItem>
  );
}

function AgentMark({ agent }: { agent: Pick<AgentStatus, "id" | "name"> }): React.JSX.Element {
  const icon = AGENT_ICONS[agent.id];
  return (
    <span className={agent.id === "oh-my-pi" ? "mark mark-oh-my-pi" : "mark"} aria-hidden="true">
      {icon ? <img src={icon} alt="" /> : agent.name.slice(0, 2).toUpperCase()}
    </span>
  );
}

function AgentsView({
  pendingAgentChanges,
  agents,
  locked,
  problem,
  onSelect,
}: {
  pendingAgentChanges: Record<string, boolean>;
  agents: AgentStatus[];
  locked: boolean;
  problem?: string;
  onSelect(agent: AgentStatus, connect: boolean): void;
}): React.JSX.Element {
  const connected = agents.filter((agent) => agent.installed && agent.connected).length;
  return (
    <div className="page-body">
      {problem && <Alert variant="destructive"><AlertDescription>{problem}</AlertDescription></Alert>}

      <div className="page-toolbar">
        <p className="page-intro">Connected agents use {brand.productName} while protected. Their previous settings return when protection stops.</p>
      </div>
      <section className="group" aria-labelledby="agents-title">
        <h2 className="group-title" id="agents-title">Installed <span>{connected} connected</span></h2>
        <div className="inset">
          {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
          {sortAgents(agents.filter((agent) => agent.installed)).map((agent) => (
            <AgentRow
              pendingConnection={pendingAgentChanges[agent.id]}
              key={agent.id}
              agent={agent}
              disabled={locked}
              onSelect={(connect) => onSelect(agent, connect)}
            />
          ))}
        </div>
      </section>
      {agents.some((agent) => !agent.installed) && <section className="group" aria-labelledby="not-installed-title">
        <h2 className="group-title" id="not-installed-title">Not installed</h2>
        <div className="inset">{sortAgents(agents.filter((agent) => !agent.installed)).map((agent) => (
          <AgentRow key={agent.id} agent={agent} disabled={locked} onSelect={() => undefined} />
        ))}</div>
      </section>}
    </div>
  );
}

function AgentRow({
  pendingConnection,
  agent,
  disabled,
  compact = false,
  onSelect,
}: {
  pendingConnection?: boolean;
  agent: AgentStatus;
  disabled: boolean;
  compact?: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const name = displayAgentName(agent);
  const presence = pendingConnection !== undefined
    ? { label: pendingConnection ? "Connecting…" : "Disconnecting…", tone: "neutral" as Tone, icon: undefined }
    : !agent.installed
    ? { label: "Not installed", tone: "neutral" as Tone, icon: undefined }
    : agent.attention
      ? { label: "Needs attention", tone: "warning" as Tone, icon: TriangleAlert }
      : agent.error
        ? { label: "Error", tone: "danger" as Tone, icon: TriangleAlert }
        : agent.connected
          ? { label: "Connected", tone: "success" as Tone, icon: undefined }
          : { label: "Not connected", tone: "neutral" as Tone, icon: undefined };
  const disconnecting = pendingConnection ?? agent.recorded;
  const actionable = disconnecting || !agent.error;
  const note = agent.attention ?? agent.error;
  return (
    <><Item size={compact ? "xs" : "default"} className="agent-block">
      <AgentMark agent={agent} />
      <ItemContent className="min-w-0">
        <ItemTitle className="row-title-line flex-wrap">
          <span className="row-title">{name}</span>
          {note && pendingConnection === undefined
            ? <AgentAttention name={name} message={note} authorized={agent.authorized} action={!disabled ? agent.repairAction : undefined} onRepair={() => onSelect(agent.repairAction === "reconnect")} />
            : <StateLabel tone={presence.tone} text={presence.label} />}
        </ItemTitle>
        {agent.installed && !compact && <Hint content={agent.configPath}><ItemDescription>{homePath(agent.configPath)}</ItemDescription></Hint>}
      </ItemContent>
      <ItemActions>
      {agent.installed ? <SwitchControl
        checked={disconnecting}
        aria-busy={pendingConnection !== undefined}
        disabled={disabled || !actionable}
        label={`${disconnecting ? "Disconnect" : "Connect"} ${name}`}
        onToggle={() => onSelect(!disconnecting)}
      /> : <AgentWebsite agent={agent} />}
      </ItemActions>
    </Item><Separator className="last:hidden" /></>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  const [error, setError] = useState<string>();
  return <span><Button variant="outline" onClick={() => {
    setError(undefined);
    void desktopApi.openAgentWebsite(agent.id).catch((error: unknown) => setError(errorMessage(error)));
  }}>Website<ExternalLink size={14} aria-hidden="true" /></Button>{error && <span className="row-note" role="alert">{error}</span>}</span>;
}

function UsageView({
  state,
  agents,
  problem,
  onNotice,
  onInspect,
}: {
  state: GatewayState;
  agents: AgentStatus[];
  problem?: string;
  onNotice(text: string): void;
  onInspect(activity: RequestActivity): void;
}): React.JSX.Element {
  const [agent, setAgent] = useState("");
  const [model, setModel] = useState("");
  const [range, setRange] = useState<UsageDateSelection>({ preset: "7d" });
  const [pageSize, setPageSize] = useState(20);
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const [page, setPage] = useState<UsagePage>();
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const focusAfterPage = useRef(false);
  const requestGeneration = useRef(0);
  const currentCursor = cursors[cursors.length - 1];
  const bounds = usageDateBounds(range);
  const { since, until } = bounds;

  const load = useCallback(async () => {
    const generation = ++requestGeneration.current;
    setLoading(true);
    setError(undefined);
    try {
      const result = await desktopApi.queryUsage({
        agent: agent || undefined,
        model: model || undefined,
        since,
        until,
        cursor: currentCursor,
        limit: pageSize,
      });
      if (generation === requestGeneration.current) {
        setPage(result);
      }
    } catch (loadError) {
      if (generation === requestGeneration.current) {
        setPage(undefined);
        setError(errorMessage(loadError));
      }
    } finally {
      if (generation === requestGeneration.current) {
        setLoading(false);
        if (focusAfterPage.current) {
          focusAfterPage.current = false;
          window.requestAnimationFrame(() => document.getElementById("usage-history-title")?.focus());
        }
      }
    }
  }, [agent, model, since, until, currentCursor, pageSize]);

  useEffect(() => { void load(); }, [load, state.usageRevision]);
  const resetPagination = () => {
    setCursors([undefined]);
  };
  const agentOptions = Array.from(new Set([
    ...(agent ? [agent] : []),
    ...agents.map((entry) => entry.id),
    ...(page?.agents ?? []),
  ]));
  const modelOptions = Array.from(new Set([...(model ? [model] : []), ...(page?.models ?? [])]));
  const exportCsv = async () => {
    try {
      const path = query.has("mock")
        ? "usage.csv"
        : await save({ title: "Export Usage", defaultPath: `private-ai-proxy-usage-${new Date().toISOString().slice(0, 10)}.csv`, filters: [{ name: "CSV", extensions: ["csv"] }] });
      if (!path) return;
      const count = await desktopApi.exportUsageCsv({ agent: agent || undefined, model: model || undefined, since, until }, path);
      onNotice(`Exported ${count.toLocaleString()} usage ${count === 1 ? "record" : "records"}`);
    } catch (exportError) {
      setError(errorMessage(exportError));
    }
  };
  const clear = async () => {
    try {
      const confirmed = await desktopApi.confirm({
        title: "Clear usage history?",
        message: "This permanently deletes local usage records. Provider billing and remote receipt retention are not affected.",
        confirmLabel: "Clear History",
      });
      if (!confirmed) return;
      const count = await desktopApi.clearUsage();
      resetPagination();
      setPage(undefined);
      onNotice(`Deleted ${count.toLocaleString()} usage ${count === 1 ? "record" : "records"}`);
    } catch (clearError) {
      setError(errorMessage(clearError));
    }
  };

  return (
    <div className="usage-page">
      {(problem || error) && <Alert variant="destructive"><AlertDescription>{problem ?? error}</AlertDescription></Alert>}
      <div className="usage-toolbar" role="group" aria-label="Usage filters">
        <Field><FieldLabel htmlFor="usage-agent">Agent</FieldLabel><ChoiceSelect id="usage-agent" label="Agent" className="w-full" value={agent} onChange={(value) => { setAgent(value); resetPagination(); }} options={[{ value: "", label: "All agents" }, ...agentOptions.map((entry) => ({ value: entry, label: agentName(entry) }))]} /></Field>
        <Field><FieldLabel htmlFor="usage-model">Model</FieldLabel><ChoiceSelect id="usage-model" label="Model" className="w-full" value={model} onChange={(value) => { setModel(value); resetPagination(); }} options={[{ value: "", label: "All models" }, ...modelOptions.map((entry) => ({ value: entry, label: entry }))]} /></Field>
        <FieldSet className="time-filter min-w-0 gap-0">
          <FieldLegend variant="label" className="leading-snug">Time</FieldLegend>
          <Suspense fallback={<Button variant="outline" disabled>{usageDateLabel(range)}</Button>}><UsageDatePicker value={range} onChange={(next) => { setRange(next); resetPagination(); }} /></Suspense>
        </FieldSet>
      </div>
      <UsageStats page={page} />
      <section className="group usage-over-time" aria-labelledby="usage-chart-title">
        <h2 className="group-title" id="usage-chart-title">Usage over time <span>{usageDateLabel(range)}</span></h2>
        <UsageChart page={page} loading={loading} range={range.preset} bounds={bounds} metric={metric} onMetric={setMetric} />
      </section>
      <section className="group usage-history" aria-labelledby="usage-history-title">
        <h2 className="group-title" id="usage-history-title" tabIndex={-1}>
          Usage history
          <span aria-live="polite">{loading ? "Loading" : page ? `${page.summary.requests} records · kept on this Mac` : "Unavailable"}</span>
          <span className="group-actions">
            <IconButton label="Export usage as CSV" onClick={() => void exportCsv()}><Download size={16} /></IconButton>
            <IconButton label="Clear usage history" onClick={() => void clear()}><Trash2 size={16} /></IconButton>
          </span>
        </h2>
        <Suspense fallback={<div className="h-80" aria-busy="true" />}><UsageTable items={page?.items ?? []} loading={loading} pageIndex={cursors.length - 1} pageSize={pageSize} total={page?.summary.requests ?? 0} onInspect={onInspect} /></Suspense>
        <div className="pagination">
          <Field orientation="horizontal" className="w-auto">
            <FieldLabel htmlFor="usage-page-size">Rows per page</FieldLabel>
            <ChoiceSelect id="usage-page-size" label="Rows per page" size="sm" value={String(pageSize)} disabled={loading} onChange={(value) => { setPageSize(Number(value)); resetPagination(); }} options={[20, 50, 100].map((size) => ({ value: String(size), label: String(size) }))} />
          </Field>
          <IconButton
            label="Previous usage page"
            disabled={loading || cursors.length === 1}
            onClick={() => {
              focusAfterPage.current = true;
              setCursors((value) => value.slice(0, -1));
            }}
          ><ChevronLeft size={16} /></IconButton>
          <span role="status" aria-live="polite">
            Page {cursors.length}
            {page && page.items.length > 0
              ? ` · ${(cursors.length - 1) * pageSize + 1}-${(cursors.length - 1) * pageSize + page.items.length} of ${page.summary.requests}`
              : ""}
          </span>
          <IconButton
            label="Next usage page"
            disabled={loading || !page?.nextCursor}
            onClick={() => {
              if (!page?.nextCursor) return;
              focusAfterPage.current = true;
              setCursors((value) => [...value, page.nextCursor]);
            }}
          ><ChevronRight size={16} /></IconButton>
        </div>
      </section>
    </div>
  );
}

function UsageStats({ page }: { page?: UsagePage }): React.JSX.Element {
  const summary = page?.summary;
  const totalTokens = (summary?.inputTokens ?? 0) + (summary?.outputTokens ?? 0);
  const forwarded = Math.max(0, (summary?.requests ?? 0) - (summary?.blockedLocally ?? 0));
  const protectedRate = forwarded ? (summary?.protected ?? 0) / forwarded : 0;
  const failedOrRejected = (summary?.blockedLocally ?? 0) + (summary?.failedProof ?? 0);
  return <div className="usage-stats"><div><span>Requests</span><strong>{summary ? summary.requests.toLocaleString() : "—"}</strong><small>{summary ? `${failedOrRejected.toLocaleString()} failed or rejected` : "—"}</small></div><div><span>Tokens</span><strong>{summary ? formatTokens(totalTokens) : "—"}</strong><small>{summary ? `${formatTokens(summary.inputTokens)} in · ${formatTokens(summary.outputTokens)} out` : "—"}</small></div><div><span>Cost</span><strong>{summary ? currency(summary.costUsd) : "—"}</strong><small>Estimated from model prices</small></div><div><span>Protected</span><strong>{forwarded ? `${Math.round(protectedRate * 100)}%` : "—"}</strong><small>{summary ? `${summary.protected} of ${forwarded} answers` : "—"}</small></div></div>;
}

function Evidence({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const receiptVerified = activity.leftDevice && activity.verified === true && Boolean(activity.receiptId);
  const ReceiptIcon = !activity.leftDevice ? Ban : receiptVerified ? ShieldCheck : ShieldX;
  const failed = activity.leftDevice && (activity.status < 200 || activity.status >= 300);
  const deliveryUnconfirmed = activity.leftDevice
    && activity.verified !== false
    && !activity.receiptId
    && (activity.status === 502 || activity.status === 504);
  const notes = [
    activity.streamed ? "Streamed response." : undefined,
    activity.locallyConstrained
      ? "The verifier applied its routing policy before sending; the receipt binds those bytes."
      : undefined,
    activity.rewritten ? "The service rewrote the request before inference; the receipt records it." : undefined,
  ].filter(Boolean);
  return (
    <>
    <div className={`privacy-verdict state-${receiptVerified ? "success" : activity.leftDevice && activity.verified === false ? "danger" : "neutral"}`}>
      <ReceiptIcon size={22} aria-hidden="true" />
      <span><strong>{!activity.leftDevice ? "Request kept on this Mac" : receiptVerified ? "Signed receipt verified" : activity.verified === false ? "Receipt verification failed" : "No verified receipt"}</strong><small>{!activity.leftDevice ? "Nothing was sent to the provider. No remote receipt is needed." : activity.verified === false ? "Do not treat this response as verified. See the recorded reason below." : receiptVerified ? "The signed receipt matches the request and response bytes recorded by the verifier." : "No successful verification result is recorded for this request."}</small></span>
    </div>
    <dl className="evidence">
      <dt>Request</dt>
      <dd>
        {agentName(activity.agent)} <code>{activity.method} {activity.path}</code>
      </dd>
      {activity.model && <><dt>Model</dt><dd><code>{activity.model}</code></dd></>}
      <dt>Outcome</dt>
      <dd>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        {failed && <span className="dim"> HTTP {activity.status}</span>}
      </dd>
      <dt>Network</dt>
      <dd>
        {!activity.leftDevice
          ? "Blocked locally; request content did not leave this Mac."
          : deliveryUnconfirmed
            ? "The request entered upstream delivery; whether the service received it could not be confirmed."
            : "Forwarded to the attested service."}
      </dd>
      <dt>Usage</dt>
      <dd>
        <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 tabular-nums">
          <dt>Input tokens</dt><dd className="text-right">{activity.inputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</dd>
          <dt>Output tokens</dt><dd className="text-right">{activity.outputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</dd>
          {activity.cacheReadTokens !== undefined && <><dt>Cache read</dt><dd className="text-right">{activity.cacheReadTokens.toLocaleString()}</dd></>}
          {activity.cacheWriteTokens !== undefined && <><dt>Cache write</dt><dd className="text-right">{activity.cacheWriteTokens.toLocaleString()}</dd></>}
          {activity.costUsd !== undefined && <><dt>Cost</dt><dd className="text-right">{currency(activity.costUsd)}</dd></>}
        </dl>
      </dd>
      {activity.receiptId && (
        <>
          <dt>Receipt ID</dt>
          <dd><code>{activity.receiptId}</code></dd>
        </>
      )}
      {notes.length > 0 && (
        <>
          <dt>Notes</dt>
          <dd>{notes.join(" ")}</dd>
        </>
      )}
    </dl>
    {activity.detail && <section className="proof-explanation" aria-label="Verification details"><h3>Verification details</h3><p className="break-words whitespace-pre-wrap">{activity.detail}</p></section>}
    {activity.leftDevice && <section className="proof-explanation" aria-label="Proof scope">
      <h3>What the proof checks</h3>
      <p>The verifier checks the request digest, the service signature against its attested keyset, and the response digest. This verifies the exchanged data, not answer accuracy.</p>
      <p>Only the verification result and receipt ID are saved here, not the full signed receipt.</p>
    </section>}
    </>
  );
}

function UsageEvidenceSheet({ activity, onClose }: { activity: RequestActivity; onClose(): void }): React.JSX.Element {
  return (
    <Sheet title="Usage proof" className="usage-evidence-sheet" headingClassName="usage-proof-heading" description={formatTimestamp(activity.at * 1_000, true)} onClose={onClose}>
      <div className="proof-card"><Evidence activity={activity} /></div>
      <DismissSheetAction onClose={onClose} />
    </Sheet>
  );
}

function CliRegistrationControl(): React.JSX.Element {
  const [registration, setRegistration] = useState<CliRegistration>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let active = true;
    void desktopApi.getCliRegistration().then(
      (status) => {
        if (!active) return;
        setRegistration(status);
      },
      (loadError: unknown) => active && setError(errorMessage(loadError)),
    );
    return () => { active = false; };
  }, []);

  const change = async () => {
    if (!registration || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      setRegistration(await desktopApi.setCliRegistration(!registration.installed));
    } catch (changeError) {
      setError(errorMessage(changeError));
    } finally {
      setBusy(false);
    }
  };

  const directory = registration ? parentDirectory(registration.commandPath) : undefined;
  const description = error
    ?? registration?.startupError
    ?? (registration?.installed
      ? registration.onPath
        ? `Installed at ${directory}. This app can resolve pap; terminal PATH may differ.`
        : `Installed at ${directory}. Ensure this directory is in your terminal PATH.`
      : directory ? `Default location: ${directory}` : "Command registration is unavailable.");
  return <Item>
    <ItemContent>
      <ItemTitle>pap command</ItemTitle>
      <ItemDescription>{description}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <Button variant="outline" disabled={busy || !registration} onClick={() => void change()}>
        {busy ? "Working…" : registration?.installed ? "Remove" : "Install"}
      </Button>
    </ItemActions>
  </Item>;
}

function SettingsView({
  updates,
  state,
  busy,
  running,
  allowDevelopmentOs,
  locked,
  problem,
  onPolicy,
  onResetSettings,
  onAboutLink,
  onOpen,
  launchPreferences,
  savingPreference,
  onLaunchPreference,
}: {
  updates: ReturnType<typeof useUpdates>;
  state: GatewayState;
  busy: boolean;
  running: boolean;
  allowDevelopmentOs: boolean;
  locked: boolean;
  problem?: string;
  onPolicy(value: boolean): void;
  onResetSettings(): void;
  onAboutLink(target: "documentation" | "github"): void;
  onOpen(target: SettingsTarget): void;
  launchPreferences?: LaunchPreferences;
  savingPreference: boolean;
  onLaunchPreference(name: keyof LaunchPreferences, enabled: boolean): void;
}): React.JSX.Element {
  const [diagnosticMessage, setDiagnosticMessage] = useState<string>();
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <div className="page-body settings-page">
      {state.wakeMonitorAvailable === false && <Alert className="border-warning/30 bg-warning/10"><AlertDescription className="text-warning">System wake monitoring is unavailable. Reconnect protection manually after sleep until monitoring recovers.</AlertDescription></Alert>}
      {problem && <Alert variant="destructive"><AlertDescription>{problem}</AlertDescription></Alert>}

      {state.endpointError && <Alert className="border-warning/30 bg-warning/10"><AlertDescription className="text-warning">{state.endpointError}</AlertDescription></Alert>}

      <SettingsSection title="General">
          <SettingsToggle label="Open at Login" checked={launchPreferences?.openAtLogin ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("openAtLogin", !launchPreferences?.openAtLogin)} />
          <SettingsToggle label="Protect on launch" checked={launchPreferences?.connectOnLaunch ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("connectOnLaunch", !launchPreferences?.connectOnLaunch)} />
          <AppearanceControl />
          <SettingsLink title="Notifications" aria-label="Notifications" aria-haspopup="dialog" onClick={() => onOpen("notifications")} />
      </SettingsSection>
      <SettingsSection title="Connections">
          <SettingsLink title="Profiles" aria-label="Profiles" aria-haspopup="dialog" onClick={() => onOpen("confidential")} description={activeProfile ? `${activeProfile.name} · ${serviceHost(activeProfile.remoteUrl)} · ${isProtected(state) ? "Protected" : profileIsAvailable(activeProfile, state) ? "Ready" : "Verification required"}` : "No provider configured"} />
          <SettingsLink title="Local API" description="Listener and client access" aria-label="Local API settings" aria-haspopup="dialog" onClick={() => onOpen("local-api")} />
      </SettingsSection>

      <Collapsible className="group settings-advanced">
        <CollapsibleTrigger render={<Button variant="ghost" />}><ChevronRight size={15} aria-hidden="true" /><span>Advanced</span></CollapsibleTrigger>
        <CollapsibleContent>
          <SettingsList>
          <SettingsToggle label="Allow development OS" checked={allowDevelopmentOs} developmentMode={allowDevelopmentOs} disabled={locked} onToggle={() => onPolicy(!allowDevelopmentOs)} />
          <UpdateChannelControl updates={updates} />
          <CliRegistrationControl />
          <ExportDiagnostics api={desktopApi} onMessage={setDiagnosticMessage} />
          <SettingsLink title="Reset settings" disabled={locked} onClick={onResetSettings} />
          </SettingsList>
        </CollapsibleContent>
      </Collapsible>


      <SettingsSection title="About">
          <UpdateControl updates={updates} productName={brand.productName} />
          {([ ["documentation", "Documentation"], ["github", "GitHub"] ] as const).map(([target, label]) => <SettingsLink key={target} title={label} external onClick={() => onAboutLink(target)} />)}
      </SettingsSection>
      {diagnosticMessage && <p role="status" className="text-sm text-muted-foreground">{diagnosticMessage}</p>}
    </div>
  );
}

function ProfilesSheet({
  state,
  busy,
  running,
  initialEditorProfileId,
  onVerify,
  onActivate,
  onDelete,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  initialEditorProfileId?: string;
  onVerify(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onActivate(profileId: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [editor, setEditor] = useState<{ kind: "new" } | { kind: "edit"; profileId: string } | undefined>(() => {
    if (state.profiles.length === 0) return { kind: "new" };
    return initialEditorProfileId ? { kind: "edit", profileId: initialEditorProfileId } : undefined;
  });
  const completeEditor = () => setEditor(undefined);
  const [openError, setOpenError] = useState<string>();
  const openEditor = (profileId?: string) => {
    if (previewMode) {
      setEditor(profileId ? { kind: "edit", profileId } : { kind: "new" });
      return;
    }
    setOpenError(undefined);
    void desktopApi.openNativeDialog("profile-editor", { profileId }).catch((error: unknown) => setOpenError(errorMessage(error)));
  };
  return (
    <>
      {state.profiles.length > 0 && (
        <ProfileListSheet
          state={state}
          busy={busy}
          running={running}
          onActivate={onActivate}
          onNew={() => openEditor()}
          onEdit={openEditor}
          error={openError}
          onClose={onClose}
        />
      )}
      {(editor || state.profiles.length === 0) && (
        <ProfileEditorSheet
          state={state}
          busy={busy}
          running={running}
          profile={editor?.kind === "edit" ? state.profiles.find((profile) => profile.id === editor.profileId) : undefined}
          onVerify={onVerify}
          onDelete={onDelete}
          onComplete={state.profiles.length === 0 ? onClose : completeEditor}
          onDeleted={state.profiles.length === 1 ? onClose : completeEditor}
          onClose={state.profiles.length === 0 ? onClose : completeEditor}
        />
      )}
    </>
  );
}

function ProfileListSheet({
  state,
  busy,
  running,
  onActivate,
  onNew,
  onEdit,
  onClose,
  error: openError,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  onActivate(profileId: string): Promise<string | undefined>;
  onNew(): void;
  onEdit(profileId: string): void;
  onClose(): void;
  error?: string;
}): React.JSX.Element {
  const [transferBusy, setTransferBusy] = useState(false);
  const [transferMessage, setTransferMessage] = useState<string>();
  const frozen = busy || transferBusy;
  const [workingProfileId, setWorkingProfileId] = useState<string>();
  const [error, setError] = useState<string>();
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const activeProfileAvailable = profileIsAvailable(activeProfile, state);

  const activate = async (profileId: string): Promise<boolean> => {
    if (profileId === state.activeProfileId) return true;
    setWorkingProfileId(profileId);
    setError(undefined);
    const message = await onActivate(profileId);
    setWorkingProfileId(undefined);
    if (message) {
      setError(message);
      return false;
    }
    return true;
  };
  const select = async (profileId: string) => {
    if (!await activate(profileId)) return;
    onClose();
  };
  return (
    <Sheet title="Profiles" className="profiles-sheet" dismissible={!workingProfileId && !transferBusy} onClose={onClose}>
      <p className="sheet-text">Choose the verified service and credential used when protection starts.</p>
      {!activeProfileAvailable && (
        <p className="banner sheet-banner profile-availability">
          <TriangleAlert size={15} aria-hidden="true" />
          {activeProfile ? `“${activeProfile.name}” cannot start protection until it is verified with an available credential.` : "Choose a verified profile before starting protection."}
        </p>
      )}
      <div className="profile-list" role="list" aria-label="AI service profiles">
        {state.profiles.map((profile) => {
          const active = profile.id === state.activeProfileId;
          const working = profile.id === workingProfileId;
          const status = !profileHasCredential(profile)
            ? "Credential unavailable"
            : profile.verifiedAt
              ? "Ready"
              : "Verification required";
          return (
            <div className={`profile-list-row${active ? " is-active" : ""}`} role="listitem" key={profile.id}>
              <ActionItem
                type="button"
                className="profile-select"
                aria-pressed={active}
                disabled={frozen || Boolean(workingProfileId)}
                onClick={() => void select(profile.id)}
              >
                <ServiceLogo url={profile.remoteUrl} size="large" />
                <span><strong>{profile.name}</strong><small>{serviceHost(profile.remoteUrl)} · {status}</small></span>
                {working ? <LoaderCircle className="is-spinning" size={16} aria-hidden="true" /> : active ? <Check size={16} aria-hidden="true" /> : null}
              </ActionItem>
              <IconButton size="icon-sm" label={`Edit ${profile.name}`} disabled={frozen || Boolean(workingProfileId)} onClick={() => onEdit(profile.id)}><Pencil /></IconButton>
            </div>
          );
        })}
      </div>
      {(error || openError) && <Alert variant="destructive"><AlertDescription>{error || openError}</AlertDescription></Alert>}
      {transferMessage && <p role="status" className="text-sm text-muted-foreground">{transferMessage}</p>}
      <SheetActions leading={
        <div className="flex items-center gap-2">
        <Button type="button" variant="outline" disabled={frozen || Boolean(workingProfileId)} onClick={onNew}><Plus size={15} />New Profile</Button>
        <ProfileTransfer api={desktopApi} disabled={busy || Boolean(workingProfileId)} onBusy={setTransferBusy} onMessage={(message, failed) => { setError(failed ? message : undefined); setTransferMessage(failed ? undefined : message); }} />
        </div>
      }>
        <Button type="button" variant="outline" disabled={Boolean(workingProfileId) || transferBusy} onClick={onClose}>Done</Button>
      </SheetActions>
    </Sheet>
  );
}

function ProfileEditorSheet({
  state,
  busy,
  running,
  profile,
  onVerify,
  onDelete,
  onComplete,
  onDeleted,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  profile?: ConfidentialProfile;
  onVerify(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onComplete(): void;
  onDeleted(): void;
  onClose(): void;
}): React.JSX.Element {
  const frozen = busy;
  const isNew = !profile;
  const [draft, setDraft] = useState<ConfidentialProfileInput>(() => ({
    id: profile?.id ?? `profile-${crypto.randomUUID()}`,
    name: profile?.name ?? "Phala",
    provider: profile?.provider ?? "phala",
    remoteUrl: profile?.remoteUrl ?? "https://inference.phala.com",
  }));
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const selectedPreset = SERVICE_PRESETS.find((service) => service.id === draft.provider);
  const keyLabel = selectedPreset?.keyLabel ?? "API key";
  const draftUrl = draft.remoteUrl.trim().replace(/\/$/, "");
  const profileChanged = !profile
    || profile.provider !== draft.provider
    || profile.remoteUrl.replace(/\/$/, "") !== draftUrl;
  const savedCredentialApplies = !isNew
    && profileHasCredential(profile)
    && !profileChanged;

  const chooseService = (next: ServicePreset) => {
    const preset = SERVICE_PRESETS.find((service) => service.id === next);
    setDraft((current) => ({
      ...current,
      provider: next,
      name: current.name === (SERVICE_PRESETS.find((service) => service.id === current.provider)?.name ?? "Custom")
        ? preset?.name ?? "Custom"
        : current.name,
      remoteUrl: preset?.url ?? (servicePreset(current.remoteUrl) ? "" : current.remoteUrl),
    }));
    setApiKeyDraft("");
    setError(undefined);
  };
  const removeProfile = async () => {
    if (saving || frozen) return;
    setSaving(true);
    setError(undefined);
    try {
      const current = await desktopApi.getState();
      const needsStop = !current.configurationVerification && ["verified", "blocked", "verifying"].includes(current.status);
      const confirmed = await desktopApi.confirm({
        title: `Delete “${draft.name}”?`,
        message: needsStop ? "Protection will stop and connected agent configurations will be restored. This profile and its saved credential will be permanently deleted." : "The profile and its saved credential will be permanently removed from this device.",
        confirmLabel: needsStop ? "Stop and Delete" : "Delete Profile",
      });
      if (!confirmed) return;
      if (needsStop) await desktopApi.stop();
      const message = await onDelete(draft.id);
      if (message) setError(message);
      else onDeleted();
    } catch (error) { setError(errorMessage(error)); }
    finally { setSaving(false); }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    try {
      if (running && profile?.id === state.activeProfileId && !await desktopApi.confirm({ title: "Save and reconnect?", message: "Saving this active profile restarts protection. In-flight requests may be interrupted.", confirmLabel: "Save and Reconnect" })) return;
      const message = await onVerify(draft, apiKeyDraft.trim() || undefined);
      if (message) setError(message);
      else onComplete();
    } catch (error) { setError(errorMessage(error)); }
    finally { setSaving(false); }
  };
  return (
    <Sheet title={isNew ? "New Profile" : "Edit Profile"} label={isNew ? "New profile" : "Edit profile"} className="profile-editor-sheet form-sheet" initialFocus="field" dismissible={!saving} onClose={onClose}>
      <form className="mt-4" onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-1">
        <FieldGroup>
        <Field>
        <FieldLabel id="profile-provider-label">Provider</FieldLabel>
        <ToggleGroup variant="outline" className="service-presets" value={[draft.provider]} disabled={frozen || saving} aria-labelledby="profile-provider-label" onValueChange={([value]) => { if (value === "phala" || value === "redpill" || value === "custom") chooseService(value); }}>
          {SERVICE_PRESETS.map((service) => (
            <ToggleGroupItem key={service.id} value={service.id} className="service-preset" aria-label={service.name}>
              <ServiceLogo url={service.url} />
              <strong>{service.name}</strong>
              {draft.provider === service.id && <Check size={15} aria-hidden="true" />}
            </ToggleGroupItem>
          ))}
          <ToggleGroupItem value="custom" className="service-preset" aria-label="Custom">
            <ServiceLogo url="custom://service" />
            <strong>Custom</strong>
            {draft.provider === "custom" && <Check size={15} aria-hidden="true" />}
          </ToggleGroupItem>
        </ToggleGroup>
        </Field>
          <FormField id="profile-name" label="Profile name"><Input id="profile-name" value={draft.name} onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))} disabled={frozen || saving} autoComplete="off" /></FormField>
          <FormField id="profile-endpoint" label="Service endpoint"><Input id="profile-endpoint" value={draft.remoteUrl} onChange={(event) => setDraft((current) => ({ ...current, remoteUrl: event.target.value }))} disabled={frozen || saving} readOnly={draft.provider !== "custom"} spellCheck={false} /></FormField>
          <Field>
            <FieldLabel htmlFor="profile-key">{keyLabel}</FieldLabel>
            <Input id="profile-key" type="password" value={apiKeyDraft} onChange={(event) => setApiKeyDraft(event.target.value)} placeholder={savedCredentialApplies ? "Replace the saved key" : `Paste your ${keyLabel}`} disabled={frozen || saving} autoComplete="off" spellCheck={false} aria-describedby="profile-key-note" />
            <FieldDescription id="profile-key-note">{savedCredentialApplies ? "Using this profile's saved key. Enter a new one to replace it after verification." : profileChanged ? "A key is required for a new provider or endpoint." : "The key is stored in the system credential store and never written into agent configs."}</FieldDescription>
          </Field>
        </FieldGroup>
        </div>
        <FieldError className="mt-3">{error}</FieldError>
        <SheetActions leading={!isNew && <Button type="button" variant="destructive" disabled={saving || frozen} onClick={() => void removeProfile()}><Trash2 size={14} />Delete Profile</Button>}>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>Cancel</Button>
          <Button type="submit" variant="default" disabled={saving || busy || frozen || !draft.name.trim() || !draft.remoteUrl.trim() || (!savedCredentialApplies && !apiKeyDraft.trim())}>{saving || busy ? "Verifying…" : "Verify and Save"}</Button>
        </SheetActions>
      </form>
    </Sheet>
  );
}

function PrivacyVerificationSheet({ state, onClose }: { state: GatewayState; onClose(): void }): React.JSX.Element {
  return <Sheet title="Privacy verification" className="privacy-sheet" onClose={onClose}><PrivacyVerification state={state} /><DismissSheetAction onClose={onClose} /></Sheet>;
}

function LocalApiSheet({
  state,
  frozen,
  clientKey,
  clientKeyVisible,
  copied,
  externalError,
  onCopy,
  onToggleKey,
  onRotate,
  onSave,
  onClose,
}: {
  state: GatewayState;
  frozen: boolean;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  externalError?: string;
  onCopy(label: string, value: string): Promise<void>;
  onToggleKey(): void;
  onRotate(): Promise<void>;
  onSave(config: LocalApiConfig): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [draft, setDraft] = useState<LocalApiConfig>(state.localApi);
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const update = <Key extends keyof LocalApiConfig>(key: Key, value: LocalApiConfig[Key]) => {
    setDraft((current) => ({ ...current, [key]: value }));
    setError(undefined);
  };
  const rotateKey = async () => {
    setSaving(true);
    setError(undefined);
    try {
      const confirmed = await desktopApi.confirm({
        title: "Rotate local API key?",
        message: "The old client key will stop working immediately. Update your tools with the new key. Agent credentials do not change. In-flight requests may be interrupted.",
        confirmLabel: "Rotate key",
      });
      if (confirmed) await onRotate();
    } catch (error) {
      setError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    try {
      if (!addressKind) {
        setError("Enter a valid IPv4 or IPv6 listen address.");
        return;
      }
      if (networkAccess && !await desktopApi.confirm({
        title: "Allow network access?",
        message: `Listen on ${draft.listenAddress}:${draft.port}? The local API uses unencrypted HTTP. Only use a trusted network, and never expose this port to the internet.`,
        confirmLabel: "Allow and Save",
      })) return;
      const message = await onSave({ ...draft, allowNetworkAccess: networkAccess });
      setError(message);
      if (!message) onClose();
    } catch (saveError) {
      setError(errorMessage(saveError));
    } finally {
      setSaving(false);
    }
  };
  return (
    <Sheet title="Local API settings" className="local-api-sheet form-sheet" initialFocus="field" dismissible={!saving} onClose={onClose}>
      <form onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-4">
          <FieldGroup>
          <div className="grid grid-cols-[minmax(0,1fr)_7rem] items-start gap-4">
          <Field>
            <div className="flex min-h-5 items-center gap-2"><FieldLabel htmlFor="local-listen-address">Listen address</FieldLabel>{networkAccess && <NetworkWarning />}</div>
            <ListenAddress api={desktopApi} value={draft.listenAddress} disabled={frozen || saving} onChange={(value) => update("listenAddress", value)} />
          </Field>
          <Field>
            <FieldLabel className="min-h-5" htmlFor="local-port">Port</FieldLabel>
            <Input id="local-port" type="number" min="1024" max="65535" required value={draft.port} disabled={frozen || saving} onChange={(event) => update("port", Number(event.target.value))} />
          </Field>
          </div>
          <FormField id="local-client-host" label="Client host" description={addressKind === "unspecified" ? "Required for all-interface listeners. Use an address reachable by your clients." : "Optional host for client URLs and agent configs. Does not change the listener."}>
            <Input id="local-client-host" aria-describedby="local-client-host-note" value={draft.clientHost ?? ""} required={addressKind === "unspecified"} placeholder="Same as listen address" disabled={frozen || saving} spellCheck={false} autoComplete="off" onChange={(event) => update("clientHost", event.target.value || undefined)} />
          </FormField>
          <FieldSeparator />
          <Field>
            <FieldLabel htmlFor="local-client-key">Client key</FieldLabel>
            <InputGroup>
              <InputGroupInput id="local-client-key" className="mono" type={clientKeyVisible ? "text" : "password"} value={clientKey} readOnly />
              <InputGroupAddon align="inline-end">
                <Hint content={clientKeyVisible ? "Hide client key" : "Reveal client key"}><InputGroupButton size="icon-xs" aria-label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff /> : <Eye />}</InputGroupButton></Hint>
                <Hint content="Copy client key"><InputGroupButton size="icon-xs" aria-label="Copy client key" disabled={saving || !clientKey} onClick={() => void onCopy("Client key", clientKey)}>{copied === "Client key" ? <Check /> : <Copy />}</InputGroupButton></Hint>
                <Hint content="Rotate key"><InputGroupButton size="icon-xs" aria-label="Rotate key" disabled={frozen || saving} onClick={() => void rotateKey()}><RefreshCw /></InputGroupButton></Hint>
              </InputGroupAddon>
            </InputGroup>
            {copied === "Client key" && <FieldDescription role="status">Copied</FieldDescription>}
          </Field>
          </FieldGroup>
        </div>
        <FieldError className="mt-3">{error ?? externalError}</FieldError>
        <SheetActions leading={
          <Button type="button" variant="outline" disabled={frozen || saving} onClick={() => setDraft({ listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 })}>Use default</Button>
        }>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>{frozen ? "Done" : "Cancel"}</Button>
          <Button type="submit" variant="default" disabled={frozen || saving}>{saving ? "Saving…" : "Save"}</Button>
        </SheetActions>
      </form>
    </Sheet>
  );
}

/** The three facts behind "Protected", each shown only when it holds now. */
function PrivacyVerification({ state }: { state: GatewayState }): React.JSX.Element {
  const verified = hasLiveVerification(state);
  const identity = state.identity;
  const checks = state.checks;
  const passed = (id: string) => checks.some((check) => check.id === id && check.status === "pass");
  const proofs = state.activity.filter((item) => item.receiptId);
  const provenProofs = proofs.filter((item) => item.verified === true).length;
  const failedProofs = proofs.filter((item) => item.verified === false).length;
  const facts: { ok: boolean; title: string; detail: string }[] = [
    {
      ok: verified && passed("id-6"),
      title: "Attested encrypted channel",
      detail: verified
        ? "Requests leave this Mac only over an SPKI-pinned TLS channel whose key is bound to the verified service identity."
        : "No verified connection is active.",
    },
    {
      ok: verified && identity?.trustLevel === "hardware_verified",
      title: "Service identity",
      detail: verified && identity
        ? `Hardware attestation checked: ${hardwareName(identity.teeType)}, ${trustName(identity.trustLevel).toLowerCase()}, built from source ${identity.source.repoCommit ? shorten(identity.source.repoCommit, 11) : "(unknown)"}.`
        : "No current identity verification. Any retained evidence below is historical.",
    },
    {
      ok: proofs.length > 0 && provenProofs === proofs.length,
      title: "Individual response receipts",
      detail: proofs.length
        ? `${provenProofs} verified · ${failedProofs} failed · ${proofs.length - provenProofs - failedProofs} unknown. Recent receipts only; open Usage for individual requests.`
        : "No recent receipts. Each request is verified separately in Usage.",
    },
  ];
  return (
    <section className="privacy-content" aria-label="Privacy">
      <div className={`privacy-verdict state-${verified ? "success" : state.status === "blocked" || state.status === "error" ? "danger" : "neutral"}`}>
        {verified ? <ShieldCheck size={22} aria-hidden="true" /> : <ShieldX size={22} aria-hidden="true" />}
        <span><strong>{verified ? "Service identity and connection verified" : state.status === "verifying" ? "Checking the service" : "No verified live connection"}</strong><small>{verified ? "This app checked the service's hardware evidence and bound the encrypted connection to its attested key." : "A saved profile is not evidence of a currently protected connection. Protection must establish a new verified session."}</small></span>
      </div>
      <div className="sheet-card privacy-facts">
        {facts.map((fact) => (
          <div className="row fact" key={fact.title}>
            <span className={fact.ok ? "check-icon check-pass" : "check-icon check-skip"} aria-hidden="true">
              {fact.ok ? <Check size={12} /> : <LockOpen size={11} />}
            </span>
            <span className="row-main">
              {fact.title}
              <span className="row-note">{fact.detail}</span>
            </span>
          </div>
        ))}
      </div>
      <p className="proof-boundary">{passed("id-5") ? "Key custody evidence passed." : "Key custody is not independently established by these checks."} This summary does not verify upstream inference or answer accuracy. {!state.config.requireProductionOs && "Development OS images are allowed."}</p>
      {identity && (
        <section className="privacy-section" aria-labelledby="verified-identity-title">
          <div className="privacy-section-heading"><h3 id="verified-identity-title">{verified ? "Current service identity" : "Last reported identity"}</h3><span>{checkCount(checks)} checks passed</span></div>
          <div className="sheet-card identity-grid">
            <Detail label="Hardware" value={hardwareName(identity.teeType)} />
            <Detail label="Trust" value={trustName(identity.trustLevel)} />
            <Detail label="Source commit" value={identity.source.repoCommit ?? "Unknown"} mono wide />
            <Detail label="Valid until" value={formatTimestamp(identity.keysetNotAfter * 1_000, true)} />
            <Detail label="Serving mode" value={identity.serving} />
            <Detail label="Channel" value={verified && passed("id-6") ? "SPKI-pinned attested TLS" : "Not established"} />
            <Detail label="Keyset digest" value={identity.keysetDigest} mono wide />
            {identity.tlsSpki && <Detail label="TLS public key" value={identity.tlsSpki} mono wide />}
            {identity.source.repoUrl && <Detail label="Source repository" value={identity.source.repoUrl} mono wide />}
            {identity.source.imageDigest && <Detail label="Image digest" value={identity.source.imageDigest} mono wide />}
          </div>
        </section>
      )}
      {checks.length > 0 && (
        <section className="privacy-section" aria-labelledby="verification-checks-title">
          <div className="privacy-section-heading"><h3 id="verification-checks-title">Verification checks</h3><span>{checks.length} total</span></div>
          <div className="sheet-card check-list">{checks.map((check) => <CheckRow key={check.id} check={check} />)}</div>
        </section>
      )}
    </section>
  );
}

function Detail({
  label,
  value,
  mono = false,
  wide = false,
}: {
  label: string;
  value: string;
  mono?: boolean;
  wide?: boolean;
}): React.JSX.Element {
  return (
    <div className={wide ? "wide" : undefined}>
      <span>{label}</span>
      <strong className={mono ? "mono" : undefined}>{value}</strong>
    </div>
  );
}

function CheckRow({ check }: { check: VerificationCheck }): React.JSX.Element {
  const title = CHECK_TITLES[check.id] ?? check.title;
  return (
    <div className="row check-row">
      <span className={`check-icon check-${check.status}`} aria-hidden="true">
        {check.status === "pass" && <Check size={12} />}
      </span>
      <span className="row-main"><span className="row-title">{title}</span><span className="row-note">{check.detail}</span></span>
      <span className={`result result-${check.status}`}>{checkStatusLabel(check.status)}</span>
    </div>
  );
}

function EmptyState({ text }: { text: string }): React.JSX.Element {
  return <div className="empty-state">{text}</div>;
}

// One headline, one line of detail, one tone: the protection status.
function presentation(state: GatewayState): {
  title: string;
  detail: string;
  tone: Tone;
  /** A Settings shortcut when the fix lives there. */
  settings?: string;
} {
  if (state.reconnecting) {
    return { title: "Reconnecting", detail: state.error ?? "Requests are paused until verification succeeds. You can cancel reconnection with the switch.", tone: "neutral" };
  }
  if (state.endpointError) {
    return {
      title: "Not protected",
      detail: `The Local API on port ${state.localApi.port} is unavailable. Check Local API settings and save to retry.`,
      tone: "danger",
    };
  }
  switch (state.status) {
    case "verifying":
      return state.configurationVerification
        ? { title: "Verifying configuration…", detail: state.progress ?? "Checking the candidate service without enabling forwarding.", tone: "neutral" }
        : { title: "Verifying…", detail: state.progress ?? "Checking the service before anything is sent.", tone: "neutral" };
    case "blocked":
      return { title: "Protection blocked", detail: state.error ?? "The verified identity or policy changed. Forwarding is fail-closed until a new verification succeeds.", tone: "danger" };
    case "error":
      return { title: "Protection interrupted", detail: state.error ?? "Forwarding is paused. Check the connection and profile, then start protection again.", tone: "danger", settings: "Open Settings" };
    case "stopped":
      return { title: "Not protected", detail: "Start to verify the service and route your agents through it.", tone: "neutral" };
    case "verified":
      if (state.configurationVerification) {
        return { title: "Configuration verified", detail: "The endpoint and credential are verified. Protection remains off until you start it.", tone: "neutral" };
      }
      if (!state.apiKeySaved) {
        return { title: "API key needed", detail: `The service is verified. Add your ${serviceKeyLabel(state.config.remoteUrl)} to start sending requests.`, tone: "warning", settings: "Add API key" };
      }
      return { title: "Protected", detail: "Requests use an SPKI-pinned TLS channel to a verified confidential AI service, with signed response proofs.", tone: "success" };
  }
}

function displayAgentName(agent: Pick<AgentStatus, "id" | "name">): string {
  return agent.id === "hermes" ? "Hermes Agent" : agent.name;
}

function sortAgents(agents: AgentStatus[]): AgentStatus[] {
  const order = ["claude-code", "codex", "hermes", "pi", "oh-my-pi", "opencode", "openclaw"];
  return [...agents].sort((left, right) => {
    const leftIndex = order.indexOf(left.id);
    const rightIndex = order.indexOf(right.id);
    return (leftIndex < 0 ? order.length : leftIndex) - (rightIndex < 0 ? order.length : rightIndex);
  });
}

function maskClientKey(key: string): string {
  if (!key) return "Unavailable";
  const prefix = key.startsWith("sk-pap-") ? "sk-pap-" : "";
  return `${prefix}${"•".repeat(12)}`;
}

function checkStatusLabel(status: VerificationCheck["status"]): string {
  switch (status) {
    case "pass": return "Pass";
    case "fail": return "Fail";
    case "skip": return "Skipped";
    case "info": return "Note";
  }
}

function checkCount(checks: VerificationCheck[]): string {
  return `${checks.filter((check) => check.status === "pass").length}/${checks.length}`;
}

function localEndpoint(config: LocalApiConfig): string | undefined {
  const host = config.clientHost?.trim() || config.listenAddress.trim();
  if (!host || !Number.isInteger(config.port) || config.port < 1 || config.port > 65_535) return undefined;
  const wrapped = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  return `http://${wrapped}:${config.port}`;
}

function serviceHost(value: string): string {
  try {
    return new URL(value).host;
  } catch {
    return value;
  }
}

function homePath(value: string): string {
  return value.replace(/^\/Users\/[^/]+/, "~").replace(/^\/home\/[^/]+/, "~");
}

function hardwareName(value: string): string {
  return value.toLowerCase() === "tdx" ? "Intel TDX" : value.toUpperCase();
}

function trustName(value: string): string {
  return value === "hardware_verified" ? "Hardware verified" : value.replaceAll("_", " ");
}

function shorten(value: string, length: number): string {
  if (value.length <= length) {
    return value;
  }
  const half = Math.floor((length - 3) / 2);
  return `${value.slice(0, half)}...${value.slice(-half)}`;
}

function parentDirectory(value: string): string {
  const separator = Math.max(value.lastIndexOf("/"), value.lastIndexOf("\\"));
  return separator > 0 ? value.slice(0, separator) : value;
}

function formatTimestamp(value: number, date = false): string {
  const options: Intl.DateTimeFormatOptions = date
    ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
    : { hour: "2-digit", minute: "2-digit" };
  return new Intl.DateTimeFormat(undefined, options).format(new Date(value));
}

function errorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  if (!message || /undefined|invoke|__TAURI_INTERNALS__/i.test(message)) {
    return "Desktop bridge unavailable";
  }
  return message;
}

function NativeWindowContent(): React.JSX.Element | null {
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

function MissingUsage({ activity }: { activity: Pick<RequestActivity, "leftDevice" | "path"> }) {
  const notApplicable = !activity.leftDevice || activity.path === "/v1/messages/count_tokens";
  const explanation = !activity.leftDevice
    ? "This request was blocked locally before forwarding. There is no provider token usage to report."
    : activity.path === "/v1/messages/count_tokens"
      ? "This endpoint counts a prompt's tokens; it does not return an inference usage report."
      : "No token count was recorded. The provider may omit usage, or the response may be incomplete or too large to capture. Missing counts are not estimated.";
  return <Hint content={explanation}><span tabIndex={0} className="text-muted-foreground underline decoration-dotted underline-offset-4">{notApplicable ? "Not applicable" : "Unavailable"}</span></Hint>;
}

function WindowContent({ reset }: { reset: boolean }): React.JSX.Element {
  return query.has("native-dialog") ? <NativeWindowContent /> : <NotificationsProvider api={desktopApi}><App initialView={reset ? "settings" : "overview"} /></NotificationsProvider>;
}

export function Renderer(): React.JSX.Element {
  const [interactionError, setInteractionError] = useState("");
  const [settingsRevision, setSettingsRevision] = useState(0);
  useEffect(() => desktopApi.onSettingsReset(() => setSettingsRevision((value) => value + 1)), []);
  useEffect(() => installNativeInteractions(desktopApi, setInteractionError), []);
  return <TooltipProvider><AppearanceProvider key={settingsRevision} api={desktopApi}><DialogCloseProvider api={desktopApi}><WindowContent reset={settingsRevision > 0} /></DialogCloseProvider>{interactionError && <span className="sr-only" role="alert">{interactionError}</span>}</AppearanceProvider></TooltipProvider>;
}
