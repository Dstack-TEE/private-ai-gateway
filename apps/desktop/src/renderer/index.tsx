import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
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
  Copy,
  Download,
  Eye,
  EyeOff,
  Info,
  Laptop,
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
import piIcon from "@lobehub/icons-static-svg/icons/pi.svg";
import phalaServiceIcon from "./assets/service-phala.svg";
import redpillServiceIcon from "./assets/service-redpill.png";

import { desktopApi as liveApi, initialGatewayState } from "./desktop-api";
import { brand } from "./generated/brand";
import { mockApi } from "./mock-api";
import { UpdateControl, UpdateChannelControl, useUpdates } from "./updates";
import { Button } from "./components/ui/button";
import { Field, FieldGroup, FieldLabel, FieldDescription, FieldError } from "./components/ui/field";
import { Badge } from "./components/ui/badge";
import { Alert, AlertDescription } from "./components/ui/alert";
import { SidebarProvider, SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./components/ui/sidebar";
import { Item, ItemActions, ItemContent, ItemTitle, ItemDescription } from "./components/ui/item";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "./components/ui/collapsible";
import { Input } from "./components/ui/input";
import { IconButton, SwitchControl } from "./components/controls";
import { Sheet, SheetActions, DismissSheetAction } from "./components/sheet";
import { SettingsSection, SettingsLink, SettingsToggle, FormField } from "./components/settings";
import { NativeSelect } from "./components/ui/native-select";
import { ToggleGroup, ToggleGroupItem } from "./components/ui/toggle-group";
import type {
  AgentStatus,
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
  hermes: hermesIcon,
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
  return Boolean(profile?.verifiedAt && profileHasCredential(profile) && state.apiKeySaved);
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
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [since]);
  const seconds = since === undefined ? undefined : Math.max(0, Math.floor(now / 1_000) - since);
  const elapsed = seconds === undefined ? undefined : [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60].map((value) => String(value).padStart(2, "0")).join(":");
  return (
    <span className="protection-status">
      {active ? <ShieldCheck size={14} aria-hidden="true" /> : <ShieldX size={14} aria-hidden="true" />}
      <span aria-live="polite">{label}</span>
      {elapsed !== undefined && <time className="protection-duration" dateTime={`PT${seconds}S`} aria-label={`Protected for ${elapsed}`} title={`Protected since ${formatTimestamp((since ?? 0) * 1_000, true)}`}>{elapsed}</time>}
    </span>
  );
}

function BrandMark({ className = "", busy = false }: { className?: string; busy?: boolean }): React.JSX.Element {
  const classes = ["brand-logo", className, busy ? "is-busy" : ""].filter(Boolean).join(" ");
  return (
    <picture className={classes} aria-hidden="true">
      <source media="(prefers-color-scheme: dark)" srcSet={brand.mark.dark} />
      <img src={brand.mark.light} alt="" />
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
type SettingsTarget = "confidential" | "privacy" | "local-api";
type UsageMetric = "tokens" | "cost" | "requests";
type Tone = "success" | "warning" | "danger" | "neutral";
const VIEWS: { id: View; label: string; icon: typeof LayoutGrid }[] = [
  { id: "overview", label: "Overview", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartNoAxesColumn },
  { id: "settings", label: "Settings", icon: Settings },
];

const PLAINTEXT_TRACKS = [
  'POST /v1/messages   { "model": "demo/verified-chat-01", "max_tokens": 2048, "system": "You summarize public documents.", "stream": true,',
  'event: content_block_delta   data: { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "The public compose hash matches the expected value." } }',
  '"messages": [ { "role": "user", "content": [ { "type": "text", "text": "Inspect the public dstack attestation report." } ] } ],   "tools": [ { "name": "read_attestation_report", "input_schema": { "type": "object", "properties": { "format": { "type": "string" } } } } ] }',
  'event: message_delta   data: { "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 96 } }',
  'POST /v1/responses   { "model": "demo/verified-reasoning-01", "instructions": "Return a concise JSON summary.", "store": false, "stream": true, "reasoning": { "effort": "low" },',
  'event: response.output_text.delta   data: { "type": "response.output_text.delta", "output_index": 0, "delta": "Release notes summarized in three points." }',
  '"input": [ { "role": "user", "content": [ { "type": "input_text", "text": "Summarize the public release notes." } ] } ],   "tools": [ { "type": "function", "name": "read_public_file", "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } } ] }',
  'event: response.completed   data: { "type": "response.completed", "response": { "id": "resp_demo_0902", "status": "completed", "usage": { "input_tokens": 384, "output_tokens": 96 } } }',
  'POST /v1/chat/completions   { "model": "demo/verified-chat-01", "stream": true, "messages": [ { "role": "system", "content": "You compare public hashes." }, { "role": "user", "content": "Compare the tdx_quote digest with compose_hash." } ],',
  'data: { "id": "chatcmpl_demo_0902", "object": "chat.completion.chunk", "choices": [ { "index": 0, "delta": { "content": "Both digests match." }, "finish_reason": null } ] }',
  '"tools": [ { "type": "function", "function": { "name": "compare_hash", "parameters": { "type": "object", "properties": { "expected": { "type": "string" } } } } } ],   "tool_choice": "auto" }',
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

function useNativeGatewayWindow(title: string, contentReady = true): {
  state: GatewayState;
  setState: React.Dispatch<React.SetStateAction<GatewayState>>;
  loaded: boolean;
  loadError?: string;
  closed: boolean;
  close(): void;
} {
  const [state, setState] = useState<GatewayState>(initialGatewayState ?? INITIAL_STATE);
  const [loaded, setLoaded] = useState(Boolean(initialGatewayState));
  const [loadError, setLoadError] = useState<string>();
  const [closed, setClosed] = useState(false);
  const presented = useRef(false);

  useEffect(() => {
    if (!loaded || (!contentReady && !loadError) || previewMode || presented.current) return;
    let active = true;
    // The dialog's layout effect has opened it before this presentation effect runs.
    presented.current = true;
    void desktopApi.nativeDialogReady().catch((error: unknown) => {
      if (active) setLoadError(errorMessage(error));
    });
    return () => { active = false; };
  }, [loaded, contentReady, loadError]);

  useEffect(() => {
    document.title = `${title} - ${brand.productName}`;
    const root = document.documentElement;
    root.classList.add("is-native-dialog");
    root.style.setProperty("--accent-light", brand.theme.accentLight);
    root.style.setProperty("--accent-dark", brand.theme.accentDark);
    let active = true;
    const unsubscribe = desktopApi.onStateChange((nextState) => {
      if (active) setState(nextState);
    });
    void desktopApi.getState().then(
      (nextState) => {
        if (!active) return;
        setState(nextState);
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

function NativeDialogStatus({ label, error, onClose }: { label: string; error?: string; onClose(): void }): React.JSX.Element | null {
  useEffect(() => {
    if (!error) return;
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); onClose(); } };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [error, onClose]);
  // The native window remains hidden until content or an actionable error is ready.
  if (!error) return null;
  return (
    <main className="native-dialog-host native-dialog-loading" aria-label={label}>
      <TriangleAlert aria-hidden="true" />
      <span role="alert">{error}</span>
      <Button variant="outline" onClick={onClose}>Done</Button>
    </main>
  );
}

function NativeProfilesWindow({ repair, editor = false }: { repair: boolean; editor?: boolean }): React.JSX.Element {
  const native = useNativeGatewayWindow(editor ? query.get("profile") ? "Edit Profile" : "New Profile" : "Profiles");
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
  if (editor) return <main className="native-dialog-host"><ProfileEditorSheet
    state={native.state} busy={busy} running={running}
    profile={native.state.profiles.find((profile) => profile.id === query.get("profile"))}
    onVerify={(profile, key) => run(() => desktopApi.verifyConfiguration(profile, native.state.config.requireProductionOs, key))}
    onDelete={(profileId) => run(() => desktopApi.deleteProfile(profileId))}
    onClearKey={() => run(() => desktopApi.clearApiKey())}
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
        onClearKey={() => run(() => desktopApi.clearApiKey())}
        onClose={native.close}
      />
    </main>
  );
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
    const unsubscribe = desktopApi.onClientKeyChange(loadClientKey);
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
  const [activity, setActivity] = useState<RequestActivity | undefined>(() => initialGatewayState?.activity.find((item) => item.id === initialRecordId));
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

function App(): React.JSX.Element {
  const updates = useUpdates(desktopApi);
  const [view, setView] = useState<View>("overview");
  const [settingsTarget, setSettingsTarget] = useState<SettingsTarget>();
  const [profileEditorId, setProfileEditorId] = useState<string>();
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [stateLoaded, setStateLoaded] = useState(false);
  const [allowDevelopmentOs, setAllowDevelopmentOs] = useState(false);
  const [launchPreferences, setLaunchPreferences] = useState<LaunchPreferences>();
  const [savingPreference, setSavingPreference] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [agentBusy, setAgentBusy] = useState<string>();
  const [refreshingAgents, setRefreshingAgents] = useState(false);
  const [applying, setApplying] = useState(false);
  const [selectedUsage, setSelectedUsage] = useState<RequestActivity>();
  const [notice, setNotice] = useState<{ id: number; text: string }>();
  const [previewTrayOpen, setPreviewTrayOpen] = useState(false);
  const copyTimer = useRef<number | undefined>(undefined);
  const firstUsePresented = useRef(false);
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
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    void desktopApi.getClientKey().then(
      (key) => active && setClientKey(key),
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    const unsubscribeClientKey = desktopApi.onClientKeyChange(() => {
      void desktopApi.getClientKey().then(
        (key) => active && setClientKey(key),
        (error: unknown) => active && setActionError(errorMessage(error)),
      );
    });
    return () => {
      active = false;
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      unsubscribe();
      unsubscribeNavigate();
      unsubscribeClientKey();
    };
  }, []);

  useEffect(() => {
    if (!stateLoaded || state.profiles.length > 0 || firstUsePresented.current) return;
    firstUsePresented.current = true;
    if (previewMode) {
      setProfileEditorId(undefined);
      setSettingsTarget("confidential");
    } else {
      void desktopApi.openNativeDialog("profiles").catch((error: unknown) => setActionError(errorMessage(error)));
    }
  }, [stateLoaded, state.profiles.length]);

  // The form mirrors the configuration the backend will start with, so a
  // start from the tray switch shows up here too.
  const configuredPolicy = state.config.requireProductionOs;
  useEffect(() => {
    setAllowDevelopmentOs(!configuredPolicy);
  }, [configuredPolicy]);

  const loadAgents = useCallback(async () => {
    const scan = ++agentScan.current;
    try {
      const next = await desktopApi.listAgents();
      if (scan === agentScan.current) setAgents(next);
    } catch (error) {
      if (scan === agentScan.current) setActionError(errorMessage(error));
    }
  }, []);

  const refreshAgents = async () => {
    setRefreshingAgents(true);
    try {
      await loadAgents();
    } finally {
      setRefreshingAgents(false);
    }
  };

  // Agent status depends on the verified catalog, so reload with the session.
  const catalogRevision = state.catalog?.revision;
  useEffect(() => {
    let active = true;
    let timer: number | undefined;
    const refresh = async () => {
      await loadAgents();
      if (active) timer = window.setTimeout(() => void refresh(), 5_000);
    };
    void refresh();
    return () => { active = false; window.clearTimeout(timer); };
  }, [loadAgents, catalogRevision, verified]);
  useEffect(() => desktopApi.onAgentsChange(() => void loadAgents()), [loadAgents]);

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
    if (!running && !busy && !profileIsAvailable(activeProfile, state)) {
      showProfiles(Boolean(activeProfile));
      return;
    }
    void run(() =>
      running || busy ? desktopApi.stop() : desktopApi.start({ remoteUrl: state.config.remoteUrl, requireProductionOs: !allowDevelopmentOs }),
    );
  };

  const verifyConfiguration = async (profile: ConfidentialProfileInput, key?: string): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      setState(await desktopApi.verifyConfiguration(
        profile,
        !allowDevelopmentOs,
        key,
      ));
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
      setNotice({ id: Date.now(), text: "Confidential AI profile deleted" });
      return undefined;
    } catch (error) {
      const message = errorMessage(error);
      setActionError(message);
      return message;
    }
  };

  const clearActiveProfileCredential = async (): Promise<string | undefined> => {
    setActionError(undefined);
    try {
      setState(await desktopApi.clearApiKey());
      setNotice({ id: Date.now(), text: "Profile credential deleted" });
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
    const options = connect && agent.id === "codex"
      ? { defaultModel: models[0]?.id }
      : {};
    setAgentBusy(agent.id);
    setActionError(undefined);
    try {
      const preview = await desktopApi.previewAgent(agent.id, connect, options);
      await desktopApi.applyAgent(agent.id, connect, preview.revision, options);
      await loadAgents();
      setNotice({ id: Date.now(), text: `${displayAgentName(agent)} ${connect ? "connected" : "disconnected"}` });
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      setAgentBusy(undefined);
    }
  };

  const restoreAll = async () => {
    setActionError(undefined);
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: "Restore all agents?",
        message: "Every agent token will be revoked first, then every configuration managed by Private AI Gateway will be restored.",
        confirmLabel: "Restore All",
      });
    } catch (error) {
      setActionError(errorMessage(error));
      return;
    }
    if (!confirmed) return;
    setApplying(true);
    try {
      await desktopApi.disconnectAllAgents();
      await loadAgents();
      setNotice({ id: Date.now(), text: "All agent configurations restored" });
      window.setTimeout(() => document.getElementById("page-title-settings")?.focus(), 0);
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      setApplying(false);
    }
  };

  const anyRecorded = agents.some((agent) => agent.recorded);
  const problem = actionError ?? state.error;
  const locked = Boolean(agentBusy) || applying;
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
  const inspectUsage = (activity: RequestActivity) => {
    if (previewMode) {
      setSelectedUsage(activity);
      return;
    }
    void desktopApi.openNativeDialog("usage-proof", { recordId: activity.id }).catch((error: unknown) => setActionError(errorMessage(error)));
  };

  const windowContent = (
    <main className="app-shell">
      <Sidebar view={view} previewControls={previewMode} updateAvailable={Boolean(updates.info?.version)} onChange={changeView} />
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
        {view === "overview" && (
          <Overview
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
            refreshing={refreshingAgents}
            onRefresh={() => void refreshAgents()}
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
            anyRecorded={anyRecorded}
            locked={locked}
            problem={problem}
            onPolicy={setAllowDevelopmentOs}
            onRestoreAll={() => void restoreAll()}
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
          onClearKey={clearActiveProfileCredential}
          onClose={() => {
            setProfileEditorId(undefined);
            setSettingsTarget(undefined);
          }}
        />
      )}
      {settingsTarget === "privacy" && (
        <PrivacyVerificationSheet state={state} onClose={() => setSettingsTarget(undefined)} />
      )}
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
}: {
  view: View;
  previewControls: boolean;
  updateAvailable: boolean;
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
    <aside className="sidebar">
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
        <span>{brand.productName}</span>
      </div>
      <SidebarProvider className="min-h-0 flex-col">
      <nav className="w-full" aria-label="Main navigation" onKeyDown={onKeyDown}>
        <SidebarMenu>
        {VIEWS.map((entry) => {
          const Icon = entry.icon;
          return (
            <SidebarMenuItem key={entry.id}><SidebarMenuButton
              size="lg"
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
      {updateAvailable && <SidebarMenu><SidebarMenuItem><SidebarMenuButton size="lg" onClick={() => onChange("settings", true)}><Download aria-hidden="true" /><span>Update available</span></SidebarMenuButton></SidebarMenuItem></SidebarMenu>}
      </SidebarProvider>
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
        <Button variant="ghost" className={`tray-trigger${trayOpen ? " is-open" : ""}`} aria-label="Private AI Gateway menu" aria-expanded={trayOpen} onClick={onTray}>
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
}): React.JSX.Element {
  const verdict = presentation(state);
  const verifying = state.status === "verifying" && !state.configurationVerification;
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const action = verifying ? "Cancel verification" : running ? "Stop protection"
    : profileIsAvailable(activeProfile, state) ? "Start protection" : "Set Up Profile…";
  return (
    <div className="preview-tray" role="menu" aria-label="Private AI Gateway">
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
  onAgents,
  onUsage,
  onCopy,
  onToggleClientKey,
  onSelect,
  onInspect,
}: {
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
  onAgents(): void;
  onUsage(): void;
  onCopy(label: string, value: string): Promise<void>;
  onToggleClientKey(): void;
  onSelect(agent: AgentStatus, connect: boolean): void;
  onInspect(activity: RequestActivity): void;
}): React.JSX.Element {
  const protectedNow = isProtected(state);
  const localAvailable = isProtected(state) && Boolean(state.proxyUrl) && !state.endpointError;
  const recent = protectedNow ? state.activity.slice(0, 4) : [];
  return (
    <div className="overview-page">
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
      {problem && (
        <p className="banner overview-banner" role="alert">
          <TriangleAlert size={15} aria-hidden="true" /> {problem}
        </p>
      )}
      <div className="overview-grid">
        <OverviewModule title="Local API" status={<StateLabel tone={localAvailable ? "success" : "neutral"} text={localAvailable ? "Available" : "Unavailable"} />}>
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
        <OverviewModule title="Usage in this session">
          <SessionSummary summary={state.sessionUsage} active={protectedNow} />
        </OverviewModule>
        <OverviewModule title="Agents" action="View all" onAction={onAgents}>
          <div className="preview-list">
            {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
            {sortAgents(agents.filter((agent) => agent.installed)).slice(0, 4).map((agent) => (
              <AgentRow
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
              <UsageRow key={item.id} activity={item} onOpen={() => onInspect(item)} />
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
  const connected = agents.filter((agent) => agent.installed && agent.connected).length;
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const activeProfileAvailable = profileIsAvailable(activeProfile, state);
  const localApiAvailable = protectedNow && Boolean(state.proxyUrl) && !endpointDown;
  const liveVerified = hasLiveVerification(state);
  const profileStatus = !activeProfile
    ? "Not configured"
    : liveVerified
      ? "Verified"
      : state.status === "verifying"
        ? "Verifying…"
      : state.status === "blocked" || state.status === "error"
        ? "Not verified"
      : activeProfileAvailable
        ? "Not connected"
      : profileHasCredential(activeProfile)
        ? "Verification required"
        : "Credential unavailable";
  return (
    <section className={`status-surface status-${state.status} ${protectedNow ? "status-ready" : ""} ${developmentMode ? "is-development" : ""}`} aria-label="Protection status">
      <TrackLayer side="left" lines={PLAINTEXT_TRACKS} active={protectedNow} />
      <TrackLayer side="right" lines={TLS_TRACKS} active={protectedNow} />
      <div className="status-glow" aria-hidden="true" />
      <div className="status-edge status-edge-left" aria-hidden="true" />
      <div className="status-edge status-edge-right" aria-hidden="true" />

      <div className="status-segment status-local">
        <div className="status-heading"><Laptop size={18} aria-hidden="true" /><span>This Mac</span></div>
        <div className={`status-fact ${localApiAvailable ? "state-success" : ""}`}>
          <span className="status-icon" aria-hidden="true"><span className="dot" /></span>
          <span>Local API {localApiAvailable ? "available" : "unavailable"}</span>
        </div>
        <div className="status-fact"><Bot size={14} aria-hidden="true" /><span>{connected} {connected === 1 ? "agent" : "agents"} connected</span></div>
        <div className="status-agent-icons" role="group" aria-label="Installed agents">
          {sortAgents(agents.filter((agent) => agent.installed)).sort((a, b) => Number(b.connected) - Number(a.connected)).map((agent) => (
            <span className={`status-agent-icon${agent.connected ? "" : " is-disconnected"}`} key={agent.id} title={`${agent.name} · ${agent.connected ? "Connected" : "Not connected"}`}>
              {AGENT_ICONS[agent.id] ? <img src={AGENT_ICONS[agent.id]} alt={agent.name} /> : agent.name.slice(0, 1)}
            </span>
          ))}
        </div>
      </div>

      <div className="status-segment status-gateway">
        <div className="gateway-core">
          <BrandMark className="gateway-mark" busy={busy} />
          <strong>{brand.productName}</strong>
          <span className={`gateway-verdict state-${verdict.tone}`}><ProtectionStatus state={state} label={verdict.title} /></span>
          <ProtectedControl
            state={state}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            developmentMode={developmentMode}
            onToggle={onToggle}
            iconOnly
          />
        </div>
      </div>

      <div className="status-segment status-remote">
        <div className="status-heading"><ShieldCheck size={18} aria-hidden="true" /><span>Confidential AI</span></div>
        <Button variant="ghost" className="status-profile" title={activeProfile?.name ?? "Setup provider"} aria-label={activeProfile ? `Profiles: ${activeProfile.name}` : "Setup provider"} aria-haspopup="dialog" onClick={onSettings}>
          {activeProfile ? <ServiceLogo url={activeProfile.remoteUrl} /> : <Plus size={18} aria-hidden="true" />}
          <span>{activeProfile?.name ?? "Setup provider"}</span>
          {activeProfile && <ChevronDown size={14} aria-hidden="true" />}
        </Button>
        <div className={`status-fact status-profile-state ${liveVerified ? "state-success" : "state-neutral"}`}>
          {liveVerified ? <ShieldCheck size={13} aria-hidden="true" /> : <ShieldX size={13} aria-hidden="true" />}
          <span>{profileStatus}</span>
          {liveVerified && <IconButton label="Privacy verification" aria-haspopup="dialog" onClick={onPrivacy}><Info size={14} aria-hidden="true" /></IconButton>}
        </div>
      </div>
    </section>
  );
}

function TrackLayer({ side, lines, active }: { side: "left" | "right"; lines: string[]; active: boolean }): React.JSX.Element {
  return (
    <div className={`track-layer tracks-${side} ${active ? "is-active" : ""}`} aria-hidden="true">
      {lines.map((line, index) => <TrackRow key={`${side}-${line}`} text={line} reverse={index % 2 === 1} />)}
    </div>
  );
}

function TrackRow({ text, reverse }: { text: string; reverse: boolean }): React.JSX.Element {
  const row = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const node = row.current;
    const copy = node?.querySelector<HTMLElement>(".track-copy");
    if (!node || !copy) return undefined;
    const update = () => {
      const distance = copy.getBoundingClientRect().width;
      node.style.setProperty("--track-duration", `${Math.max(distance / 20, 1)}s`);
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(copy);
    return () => observer.disconnect();
  }, [text]);
  return (
    <div ref={row} className={`track-row ${reverse ? "track-reverse" : ""}`}>
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
  const checked = running || protectionStarting;
  const label = busy
    ? state.configurationVerification ? "Cancel configuration verification" : "Cancel protection start"
    : running ? "Stop protection" : "Start protection";
  return (
    <div className={`protected-control ${compact ? "is-compact" : ""} ${iconOnly && !compact ? "is-icon-only" : ""}`}>
      {!iconOnly && <span>Protected</span>}
      {developmentMode && !compact && <span className="dev-mode-label">Dev mode</span>}
      <SwitchControl
        size="default"
        checked={checked}
        label={label}
        disabled={endpointDown && !checked}
        title={endpointDown && !checked ? state.endpointError : label}
        developmentMode={developmentMode}
        onToggle={onToggle}
      />
    </div>
  );
}

function OverviewModule({
  title,
  status,
  action,
  onAction,
  children,
}: React.PropsWithChildren<{
  title: string;
  status?: React.ReactNode;
  action?: string;
  onAction?(): void;
}>): React.JSX.Element {
  return (
    <section className="overview-module">
      <header className="overview-module-title">
        <h2>{title}</h2>
        {status}
        {action && onAction && <Button variant="ghost" size="xs" className="module-action" onClick={onAction}>{action}</Button>}
      </header>
      <div className="module inset">{children}</div>
    </section>
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
      <div className="copy-row">
        <Button variant="ghost"
          className="copy-surface h-full w-full"
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
      </div>
      <div className="copy-row">
        <Button variant="ghost" className="copy-surface h-full w-full" disabled={!clientKey} aria-label={`${keyLabel}: ${clientKeyVisible ? clientKey : "hidden"}. Copy`} onClick={() => clientKey && void onCopy(keyLabel, clientKey)}>
          <span className="row-title-line">
            <span className="row-title">Client key</span>
          </span>
          <code className="row-note">{clientKey ? clientKeyVisible ? clientKey : maskClientKey(clientKey) : "Unavailable"}</code>
          <span className={`copy-feedback ${copied === keyLabel ? "is-copied" : ""}`}>{copied === keyLabel ? "Copied" : "Copy"}</span>
        </Button>
        <IconButton className="row-action" label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
      </div>
      {endpointError && <p className="inline-error">{endpointError}</p>}
    </div>
  );
}

function SessionSummary({ summary, active }: { summary: UsageSummary; active: boolean }): React.JSX.Element {
  const forwarded = Math.max(0, summary.requests - summary.blockedLocally);
  const totalTokens = summary.inputTokens + summary.outputTokens;
  const protectedRate = forwarded ? Math.round((summary.protected / forwarded) * 100) : 0;
  return (
    <div className="session-summary">
      <div><span>Requests</span><strong>{active ? summary.requests.toLocaleString() : "—"}</strong></div>
      <div><span>Tokens</span><strong>{active ? formatTokens(totalTokens) : "—"}</strong></div>
      <div><span>Cost</span><strong>{active ? currency(summary.costUsd) : "—"}</strong></div>
      <div><span>Protected</span><strong>{active && forwarded ? `${protectedRate}%` : "—"}</strong></div>
    </div>
  );
}

function UsageRow({ activity, onOpen }: { activity: RequestActivity; onOpen(): void }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const tokens = (activity.inputTokens ?? 0) + (activity.outputTokens ?? 0);
  const timestamp = new Date(activity.at * 1_000);
  return (
    <Button variant="ghost" className="row list-row usage-row" onClick={onOpen} aria-label={`${agentName(activity.agent)}, ${outcome.label}, ${activity.model ?? activity.path}. View proof`}>
      <span className="row-main">
        <span className="row-title">{agentName(activity.agent)}</span>
        <StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} />
        <code className="row-note">{activity.model ?? activity.path}</code>
      </span>
      <span className="usage-amount"><strong>{tokens ? formatTokens(tokens) : "—"}</strong><small>tokens</small></span>
      <span className="usage-amount usage-cost"><strong>{activity.costUsd === undefined ? "—" : currency(activity.costUsd)}</strong><small>cost</small></span>
      <time className="row-side" dateTime={timestamp.toISOString()} title={formatTimestamp(timestamp.getTime(), true)}><span>{timestamp.toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span><span>{formatTimestamp(timestamp.getTime())}</span></time>
    </Button>
  );
}

function AgentMark({ agent }: { agent: Pick<AgentStatus, "id" | "name"> }): React.JSX.Element {
  const icon = AGENT_ICONS[agent.id];
  return (
    <span className="mark" aria-hidden="true">
      {icon ? <img src={icon} alt="" /> : agent.name.slice(0, 2).toUpperCase()}
    </span>
  );
}

function AgentsView({
  refreshing,
  onRefresh,
  agents,
  locked,
  problem,
  onSelect,
}: {
  refreshing: boolean;
  onRefresh(): void;
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
        <IconButton label="Detect installed agents" disabled={refreshing} onClick={onRefresh}><RefreshCw className={refreshing ? "is-spinning" : undefined} aria-hidden="true" /></IconButton>
      </div>
      <section className="group" aria-labelledby="agents-title">
        <h2 className="group-title" id="agents-title">Installed <span>{connected} connected</span></h2>
        <div className="inset">
          {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
          {sortAgents(agents.filter((agent) => agent.installed)).map((agent) => (
            <AgentRow
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
      <p className="page-footnote">Available models sync automatically from the verified service.</p>
    </div>
  );
}

function AgentRow({
  agent,
  disabled,
  compact = false,
  onSelect,
}: {
  agent: AgentStatus;
  disabled: boolean;
  compact?: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const name = displayAgentName(agent);
  const presence = !agent.installed
    ? { label: "Not installed", tone: "neutral" as Tone, icon: undefined }
    : agent.attention
      ? { label: "Needs attention", tone: "warning" as Tone, icon: TriangleAlert }
      : agent.error
        ? { label: "Error", tone: "danger" as Tone, icon: TriangleAlert }
        : agent.connected
          ? { label: "Connected", tone: "success" as Tone, icon: ShieldCheck }
          : { label: "Not connected", tone: "neutral" as Tone, icon: undefined };
  const disconnecting = agent.recorded;
  const actionable = disconnecting || !agent.error;
  const note = agent.attention ?? agent.error;
  return (
    <Item size={compact ? "xs" : "default"} className="agent-block" title={agent.configPath}>
      <span className={agent.connected ? "agent-mark-on" : undefined}><AgentMark agent={agent} /></span>
      <ItemContent className="min-w-0">
        <ItemTitle className="row-title-line flex-wrap">
          <span className="row-title">{name}</span>
          <StateLabel tone={presence.tone} icon={presence.icon} text={presence.label} />
        </ItemTitle>
        {agent.installed && !compact && <ItemDescription title={agent.configPath}>{homePath(agent.configPath)}</ItemDescription>}
        {note && <ItemDescription>{note}</ItemDescription>}
      </ItemContent>
      <ItemActions>
      {agent.installed ? <SwitchControl
        checked={disconnecting}
        disabled={disabled || !actionable}
        label={`${disconnecting ? "Disconnect" : "Connect"} ${name}`}
        onToggle={() => onSelect(!disconnecting)}
      /> : <AgentWebsite agent={agent} />}
      </ItemActions>
    </Item>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  const [error, setError] = useState<string>();
  return <span><Button variant="outline" onClick={() => {
    setError(undefined);
    void desktopApi.openAgentWebsite(agent.id).catch((error: unknown) => setError(errorMessage(error)));
  }}>Website<ExternalLink size={14} aria-hidden="true" /></Button>{error && <span className="row-note" role="alert">{error}</span>}</span>;
}

/** Text plus a tone icon, so no state relies on colour alone. */
function StateLabel({
  tone,
  icon: Icon,
  text,
}: {
  tone: Tone;
  icon?: typeof ShieldCheck;
  text: string;
}): React.JSX.Element {
  return (
    <Badge variant={tone === "danger" || tone === "warning" ? "destructive" : tone === "success" ? "secondary" : "outline"}>
      {Icon ? <Icon size={13} aria-hidden="true" /> : <span className="dot" aria-hidden="true" />}
      {text}
    </Badge>
  );
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
  const [range, setRange] = useState("7d");
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const [page, setPage] = useState<UsagePage>();
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const focusAfterPage = useRef(false);
  const requestGeneration = useRef(0);
  const currentCursor = cursors[cursors.length - 1];
  const since = usageSince(range);

  const load = useCallback(async () => {
    const generation = ++requestGeneration.current;
    setLoading(true);
    setError(undefined);
    try {
      const result = await desktopApi.queryUsage({
        agent: agent || undefined,
        model: model || undefined,
        since: usageSince(range),
        cursor: currentCursor,
        limit: 20,
      });
      if (generation === requestGeneration.current) {
        setPage(result);
      }
    } catch (loadError) {
      if (generation === requestGeneration.current) {
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
  }, [agent, model, range, currentCursor]);

  useEffect(() => { void load(); }, [load, state.usageRevision]);
  const resetPagination = () => {
    setCursors([undefined]);
  };
  const agentOptions = Array.from(new Set([
    ...agents.map((entry) => entry.id),
    ...(page?.agents ?? []),
  ]));
  const exportCsv = async () => {
    try {
      const path = query.has("mock")
        ? "usage.csv"
        : await save({ title: "Export Usage", defaultPath: `private-ai-gateway-usage-${new Date().toISOString().slice(0, 10)}.csv`, filters: [{ name: "CSV", extensions: ["csv"] }] });
      if (!path) return;
      const count = await desktopApi.exportUsageCsv({ agent: agent || undefined, model: model || undefined, since }, path);
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
        <Field><FieldLabel htmlFor="usage-agent">Agent</FieldLabel><NativeSelect id="usage-agent" value={agent} onChange={(event) => { setAgent(event.target.value); resetPagination(); }}><option value="">All agents</option>{agentOptions.map((entry) => <option key={entry} value={entry}>{agentName(entry)}</option>)}</NativeSelect></Field>
        <Field><FieldLabel htmlFor="usage-model">Model</FieldLabel><NativeSelect id="usage-model" value={model} onChange={(event) => { setModel(event.target.value); resetPagination(); }}><option value="">All models</option>{page?.models.map((entry) => <option key={entry} value={entry}>{entry}</option>)}</NativeSelect></Field>
        <fieldset className="filter-field time-filter">
          <legend>Time</legend>
          <ToggleGroup className="segmented-control" spacing={0} value={[range]} aria-label="Usage time range" onValueChange={([value]) => { if (value) { setRange(value); resetPagination(); } }}>
            {(["24h", "7d", "30d", "all"] as const).map((value) => (
              <ToggleGroupItem
                key={value}
                value={value}
              >
                {{ "24h": "Today", "7d": "7 days", "30d": "30 days", all: "All" }[value]}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </fieldset>
      </div>
      <UsageStats page={page} />
      <section className="group usage-over-time" aria-labelledby="usage-chart-title">
        <h2 className="group-title" id="usage-chart-title">Usage over time <span>{rangeLabel(range)}</span></h2>
        <UsageChart page={page} range={range} metric={metric} onMetric={setMetric} />
      </section>
      <section className="group usage-history" aria-labelledby="usage-history-title">
        <h2 className="group-title" id="usage-history-title" tabIndex={-1}>
          Usage history
          <span aria-live="polite">{loading ? "Loading" : `${page?.summary.requests ?? 0} records · kept on this Mac`}</span>
          <span className="group-actions">
            <IconButton label="Export usage as CSV" onClick={() => void exportCsv()}><Download size={16} /></IconButton>
            <IconButton label="Clear usage history" onClick={() => void clear()}><Trash2 size={16} /></IconButton>
          </span>
        </h2>
        <div className="inset list" aria-busy={loading}>
          {loading && !page && <EmptyState text="Loading usage history…" />}
          {!loading && page?.items.length === 0 && <EmptyState text={page.summary.requests === 0 ? "No saved usage matches these filters." : "No records on this page."} />}
          <ul className="list-items" aria-label="Usage history">
            {page?.items.map((item) => (
              <li key={item.id}><UsageRow activity={item} onOpen={() => onInspect(item)} /></li>
            ))}
          </ul>
        </div>
        <div className="pagination">
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
              ? ` · ${(cursors.length - 1) * 20 + 1}-${(cursors.length - 1) * 20 + page.items.length} of ${page.summary.requests}`
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
  return <div className="usage-stats"><div><span>Requests</span><strong>{(summary?.requests ?? 0).toLocaleString()}</strong><small>{failedOrRejected.toLocaleString()} failed or rejected</small></div><div><span>Tokens</span><strong>{formatTokens(totalTokens)}</strong><small>{formatTokens(summary?.inputTokens ?? 0)} in · {formatTokens(summary?.outputTokens ?? 0)} out</small></div><div><span>Cost</span><strong>{currency(summary?.costUsd ?? 0)}</strong><small>Estimated from model prices</small></div><div><span>Protected</span><strong>{forwarded ? `${Math.round(protectedRate * 100)}%` : "—"}</strong><small>{summary?.protected ?? 0} of {forwarded} answers</small></div></div>;
}

function UsageChart({
  page,
  range,
  metric,
  onMetric,
}: {
  page?: UsagePage;
  range: string;
  metric: UsageMetric;
  onMetric(metric: UsageMetric): void;
}): React.JSX.Element {
  const series = completeDailySeries(page?.series ?? [], range).slice(-30);
  const value = (point: UsagePage["series"][number]) => metric === "tokens" ? point.tokens : metric === "cost" ? point.costUsd : point.requests;
  const peak = Math.max(1, ...series.map(value));
  const labelIndexes = chartLabelIndexes(series.length);
  return (
    <figure className="usage-chart" aria-label={`${metric} usage by day`}>
      <div className="chart-toolbar">
        <ToggleGroup className="segmented-control chart-metric" spacing={0} value={[metric]} aria-label="Chart metric" onValueChange={([value]) => { if (value === "tokens" || value === "cost" || value === "requests") onMetric(value); }}>
          {(["tokens", "cost", "requests"] as const).map((entry) => (
            <ToggleGroupItem key={entry} value={entry}>
              {{ tokens: "Tokens", cost: "Cost", requests: "Requests" }[entry]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        {metric === "tokens" && <span className="chart-legend"><i className="input" />Input <i className="output" />Output</span>}
      </div>
      <div className="chart-bars" aria-hidden="true">
        {series.map((point, index) => (
          <div key={point.day} className="chart-column" title={`${point.day}: ${point.tokens.toLocaleString()} tokens, ${point.requests} requests, ${currency(point.costUsd)}`}>
            <span className={`chart-stack${value(point) === 0 ? " is-empty" : ""}`} style={{ height: `${value(point) / peak * 100}%` }}>
              {metric === "tokens" ? (
                <><i className="output" style={{ flexGrow: point.outputTokens }} /><i className="input" style={{ flexGrow: point.inputTokens }} /></>
              ) : <i className="single" />}
            </span>
            <small>{labelIndexes.has(index) ? point.day.slice(5) : ""}</small>
          </div>
        ))}
      </div>
      <ul className="sr-only">
        {series.map((point) => <li key={point.day}>{point.day}: {point.tokens.toLocaleString()} tokens, {point.requests} requests, {currency(point.costUsd)}</li>)}
      </ul>
      {series.length === 0 && <EmptyState text="No saved usage to chart for this range." />}
    </figure>
  );
}

function completeDailySeries(series: UsagePage["series"], range: string): UsagePage["series"] {
  if (series.length === 0 && range === "all") return [];
  const byDay = new Map(series.map((point) => [point.day, point]));
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const fixedDays = range === "24h" ? 1 : range === "7d" ? 7 : range === "30d" ? 30 : undefined;
  const start = fixedDays
    ? new Date(today.getFullYear(), today.getMonth(), today.getDate() - fixedDays + 1)
    : parseLocalDay(series[0]?.day) ?? today;
  const completed: UsagePage["series"] = [];
  for (const cursor = new Date(start); cursor <= today; cursor.setDate(cursor.getDate() + 1)) {
    const day = localDay(cursor);
    completed.push(byDay.get(day) ?? {
      day,
      requests: 0,
      inputTokens: 0,
      outputTokens: 0,
      tokens: 0,
      costUsd: 0,
    });
  }
  return completed;
}

function parseLocalDay(day?: string): Date | undefined {
  const match = day?.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!match) return undefined;
  return new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
}

function localDay(date: Date): string {
  return [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("-");
}

function chartLabelIndexes(length: number): Set<number> {
  if (length <= 4) {
    return new Set(Array.from({ length }, (_, index) => index));
  }
  return new Set(Array.from({ length: 4 }, (_, index) => Math.round(index * (length - 1) / 3)));
}

function Evidence({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const receiptVerified = activity.leftDevice && activity.verified === true && Boolean(activity.receiptId);
  const ReceiptIcon = !activity.leftDevice ? Ban : receiptVerified ? ShieldCheck : ShieldX;
  const failed = activity.leftDevice && (activity.status < 200 || activity.status >= 300);
  const deliveryUnconfirmed = activity.leftDevice
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
        {activity.detail && <span className="dim"> · {activity.detail}</span>}
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
        {activity.inputTokens === undefined && activity.outputTokens === undefined
          ? "Not reported"
          : `${(activity.inputTokens ?? 0).toLocaleString()} input · ${(activity.outputTokens ?? 0).toLocaleString()} output`}
        {(activity.cacheReadTokens !== undefined || activity.cacheWriteTokens !== undefined)
          && <span className="dim"> · {(activity.cacheReadTokens ?? 0).toLocaleString()} cache read · {(activity.cacheWriteTokens ?? 0).toLocaleString()} cache write</span>}
        {activity.costUsd !== undefined && <span className="dim"> · {currency(activity.costUsd)}</span>}
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

function SettingsView({
  updates,
  state,
  busy,
  running,
  allowDevelopmentOs,
  anyRecorded,
  locked,
  problem,
  onPolicy,
  onRestoreAll,
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
  anyRecorded: boolean;
  locked: boolean;
  problem?: string;
  onPolicy(value: boolean): void;
  onRestoreAll(): void;
  onAboutLink(target: "documentation" | "github"): void;
  onOpen(target: SettingsTarget): void;
  launchPreferences?: LaunchPreferences;
  savingPreference: boolean;
  onLaunchPreference(name: keyof LaunchPreferences, enabled: boolean): void;
}): React.JSX.Element {
  const frozen = busy || running;
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <div className="page-body settings-page">
      {problem && <Alert variant="destructive"><AlertDescription>{problem}</AlertDescription></Alert>}

      <SettingsSection title="General">
          <SettingsToggle label="Open at Login" checked={launchPreferences?.openAtLogin ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("openAtLogin", !launchPreferences?.openAtLogin)} />
          <SettingsToggle label="Connect on launch" description="Start protection using the selected profile." checked={launchPreferences?.connectOnLaunch ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("connectOnLaunch", !launchPreferences?.connectOnLaunch)} />
          <SettingsLink title="Profiles" aria-label="Profiles" aria-haspopup="dialog" onClick={() => onOpen("confidential")} description={activeProfile ? `${activeProfile.name} · ${serviceHost(activeProfile.remoteUrl)} · ${isProtected(state) ? "Protected" : profileIsAvailable(activeProfile, state) ? "Verified configuration" : "Verification required"}` : "No provider configured"} />
          {state.endpointError && <p className="row-warning">{state.endpointError}</p>}
          <SettingsLink title="Local API" description="Listener and client access" aria-label="Local API settings" aria-haspopup="dialog" onClick={() => onOpen("local-api")} />
      </SettingsSection>

      <Collapsible className="group settings-advanced">
        <CollapsibleTrigger render={<Button variant="ghost" />}><ChevronRight size={15} aria-hidden="true" /><span>Advanced</span></CollapsibleTrigger>
        <CollapsibleContent className="inset">
          <SettingsToggle label="Allow development OS" description={`Accept development OS images that are not intended for production workloads.${frozen ? " Stop protection to change this setting." : ""}`} checked={allowDevelopmentOs} developmentMode={allowDevelopmentOs} disabled={frozen} onToggle={() => onPolicy(!allowDevelopmentOs)} />
        </CollapsibleContent>
      </Collapsible>

      {anyRecorded && <SettingsSection title="Agents"><div className="row"><span className="row-main"><span className="row-title">Restore all agent configs</span><span className="row-note">Turns every agent off and puts every config back, even while protection is off.</span></span><Button variant="outline" disabled={locked} onClick={onRestoreAll}>Restore all</Button></div></SettingsSection>}

      <SettingsSection title="About">
          <UpdateChannelControl updates={updates} />
          <div className="row"><span className="row-main">{brand.productName}</span><UpdateControl updates={updates} /></div>
          {([ ["documentation", "Documentation"], ["github", "GitHub"] ] as const).map(([target, label]) => <SettingsLink key={target} title={label} external onClick={() => onAboutLink(target)} />)}
      </SettingsSection>
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
  onClearKey,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  initialEditorProfileId?: string;
  onVerify(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onActivate(profileId: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onClearKey(): Promise<string | undefined>;
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
          onClearKey={onClearKey}
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
  const frozen = busy;
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
    <Sheet title="Profiles" className="profiles-sheet" onClose={onClose}>
      <p className="sheet-text">Choose the verified service and credential used when protection starts.</p>
      {!activeProfileAvailable && (
        <p className="banner sheet-banner profile-availability">
          <TriangleAlert size={15} aria-hidden="true" />
          {activeProfile ? `“${activeProfile.name}” cannot start protection until it is verified with an available credential.` : "Choose a verified profile before starting protection."}
        </p>
      )}
      <div className="profile-list" role="list" aria-label="Confidential AI profiles">
        {state.profiles.map((profile) => {
          const active = profile.id === state.activeProfileId;
          const working = profile.id === workingProfileId;
          const status = !profileHasCredential(profile)
            ? "Credential unavailable"
            : profile.verifiedAt
              ? "Verified configuration"
              : "Verification required";
          return (
            <div className={`profile-list-row${active ? " is-active" : ""}`} role="listitem" key={profile.id}>
              <Button variant="ghost"
                type="button"
                className="profile-select"
                aria-pressed={active}
                disabled={frozen || Boolean(workingProfileId)}
                onClick={() => void select(profile.id)}
              >
                <ServiceLogo url={profile.remoteUrl} size="large" />
                <span><strong>{profile.name}</strong><small>{serviceHost(profile.remoteUrl)} · {status}</small></span>
                {working ? <LoaderCircle className="is-spinning" size={16} aria-hidden="true" /> : active ? <Check size={16} aria-hidden="true" /> : null}
              </Button>
              <IconButton label={`Edit ${profile.name}`} disabled={frozen || Boolean(workingProfileId)} onClick={() => onEdit(profile.id)}><Pencil size={15} /></IconButton>
            </div>
          );
        })}
      </div>
      {running && <p className="field-note profile-lock-note">Switching profiles briefly stops protection and reconnects to the selected provider.</p>}
      {(error || openError) && <Alert variant="destructive"><AlertDescription>{error || openError}</AlertDescription></Alert>}
      <SheetActions leading={
        <Button type="button" variant="outline" disabled={frozen || Boolean(workingProfileId)} onClick={onNew}><Plus size={15} />New Profile</Button>
      }>
        <Button type="button" variant="outline" onClick={onClose}>Done</Button>
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
  onClearKey,
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
  onClearKey(): Promise<string | undefined>;
  onComplete(): void;
  onDeleted(): void;
  onClose(): void;
}): React.JSX.Element {
  const frozen = busy;
  const isNew = !profile;
  const [nameEdited, setNameEdited] = useState(Boolean(profile));
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
  const verifiedConfiguration = Boolean(profile?.verifiedAt) && savedCredentialApplies && !apiKeyDraft.trim();

  const chooseService = (next: ServicePreset) => {
    const preset = SERVICE_PRESETS.find((service) => service.id === next);
    setDraft((current) => ({
      ...current,
      provider: next,
      name: nameEdited ? current.name : preset?.name ?? "Custom",
      remoteUrl: preset?.url ?? (servicePreset(current.remoteUrl) ? "" : current.remoteUrl),
    }));
    setApiKeyDraft("");
    setError(undefined);
  };
  const removeProfile = async () => {
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: `Delete “${draft.name}”?`,
        message: "The profile and its saved credential will be permanently removed from this device.",
        confirmLabel: "Delete Profile",
      });
    } catch (confirmError) {
      setError(errorMessage(confirmError));
      return;
    }
    if (!confirmed) return;
    setSaving(true);
    setError(undefined);
    const message = await onDelete(draft.id);
    setSaving(false);
    if (message) {
      setError(message);
    } else {
      onDeleted();
    }
  };
  const clearKey = async () => {
    let confirmed: boolean;
    try {
      confirmed = await desktopApi.confirm({
        title: `Delete the credential for “${draft.name}”?`,
        message: "Protection cannot start with this profile until a new credential is verified and saved.",
        confirmLabel: "Delete Credential",
      });
    } catch (confirmError) {
      setError(errorMessage(confirmError));
      return;
    }
    if (!confirmed) return;
    setSaving(true);
    setError(undefined);
    const message = await onClearKey();
    setSaving(false);
    if (message) setError(message);
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    const message = await onVerify(draft, apiKeyDraft.trim() || undefined);
    setSaving(false);
    if (message) {
      setError(message);
      return;
    }
    onComplete();
  };
  return (
    <Sheet title={isNew ? "New Profile" : "Edit Profile"} label={isNew ? "New profile" : "Edit profile"} className="profile-editor-sheet" dismissible={!saving} onClose={onClose}>
      {running && <p className="field-note">Saving briefly stops protection, verifies this profile, then reconnects. If verification fails, protection stays off.</p>}
      <form onSubmit={(event) => void submit(event)}>
        <ToggleGroup variant="outline" className="service-presets" value={[draft.provider]} disabled={frozen || saving} aria-label="Confidential AI provider" onValueChange={([value]) => { if (value === "phala" || value === "redpill" || value === "custom") chooseService(value); }}>
          {SERVICE_PRESETS.map((service) => (
            <ToggleGroupItem key={service.id} value={service.id} className="service-preset" aria-label={service.name} title={service.url}>
              <ServiceLogo url={service.url} size="large" />
              <strong>{service.name}</strong>
              {draft.provider === service.id && <Check size={15} aria-hidden="true" />}
            </ToggleGroupItem>
          ))}
          <ToggleGroupItem value="custom" className="service-preset" aria-label="Custom" title="Use another ACI endpoint">
            <ServiceLogo url="custom://service" size="large" />
            <strong>Custom</strong>
            {draft.provider === "custom" && <Check size={15} aria-hidden="true" />}
          </ToggleGroupItem>
        </ToggleGroup>
        <FieldGroup className="mt-6">
          <FormField id="profile-name" label="Profile name"><Input id="profile-name" value={draft.name} onChange={(event) => { setNameEdited(true); setDraft((current) => ({ ...current, name: event.target.value })); }} disabled={frozen || saving} autoComplete="off" /></FormField>
          <FormField id="profile-endpoint" label="Service endpoint"><Input id="profile-endpoint" value={draft.remoteUrl} onChange={(event) => setDraft((current) => ({ ...current, remoteUrl: event.target.value }))} disabled={frozen || saving || draft.provider !== "custom"} spellCheck={false} /></FormField>
          <Field>
            <FieldLabel htmlFor="profile-key">{keyLabel}</FieldLabel>
            <Input id="profile-key" type="password" value={apiKeyDraft} onChange={(event) => setApiKeyDraft(event.target.value)} placeholder={savedCredentialApplies ? "Replace the saved key" : `Paste your ${keyLabel}`} disabled={frozen || saving} autoComplete="off" spellCheck={false} aria-describedby="profile-key-note" />
            <FieldDescription id="profile-key-note">{verifiedConfiguration ? "The endpoint and credential were verified together and saved securely." : savedCredentialApplies ? "Using this profile's saved key. Enter a new one to replace it after verification." : profileChanged ? "A key is required for a new provider or endpoint." : "The key is stored in the system credential store and never written into agent configs."}</FieldDescription>
            {verifiedConfiguration && <Badge variant="secondary"><Check aria-hidden="true" />Verified configuration</Badge>}
            {savedCredentialApplies && profile?.id === state.activeProfileId && <Button className="self-start" type="button" variant="link" onClick={() => void clearKey()} disabled={saving || frozen || running}>Delete credential</Button>}
          </Field>
          <FieldError>{error}</FieldError>
        </FieldGroup>
        <SheetActions leading={!isNew && <Button type="button" variant="destructive" title={running ? "Stop protection before deleting a profile" : undefined} disabled={saving || frozen || running} onClick={() => void removeProfile()}><Trash2 size={14} />Delete Profile</Button>}>
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
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const endpoint = localEndpoint(draft) ?? "";
  const openAi = openAiEndpoint(endpoint) ?? "";
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
        message: "The old client key will stop working immediately. Update your tools with the new key. Connected agents use separate keys and are not affected.",
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
    const message = await onSave(draft);
    setSaving(false);
    setError(message);
    if (!message) onClose();
  };
  return (
    <Sheet title="Local API settings" className="local-api-sheet" dismissible={!saving} onClose={onClose}>
      <form onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-6">
          <FieldGroup>
          <FormField id="local-listen-address" label="Listen address" description="Address used by the local gateway.">
              <Input id="local-listen-address" aria-describedby="local-listen-address-note" list="listen-addresses" value={draft.listenAddress} disabled={frozen || saving} spellCheck={false} autoComplete="off" onChange={(event) => update("listenAddress", event.target.value)} />
            <datalist id="listen-addresses"><option value="127.0.0.1" /><option value="::1" /><option value="0.0.0.0" /></datalist>
          </FormField>
          <SettingsToggle label="Allow network access" description="Permit a non-loopback listen address. Keep this off for local agents." checked={draft.allowNetworkAccess} disabled={frozen || saving} onToggle={() => update("allowNetworkAccess", !draft.allowNetworkAccess)} />
          {draft.allowNetworkAccess && <p className="row-warning">Other devices on the network may reach this gateway. Only use this on a trusted network.</p>}
          <FormField id="local-port" label="Port" description="1024–65535">
            <Input id="local-port" aria-describedby="local-port-note" type="number" min="1024" max="65535" value={draft.port} disabled={frozen || saving} onChange={(event) => update("port", Number(event.target.value))} />
          </FormField>
          <FormField id="local-client-host" label="Client host" description="Optional hostname shown to clients.">
            <Input id="local-client-host" aria-describedby="local-client-host-note" value={draft.clientHost ?? ""} placeholder="Same as listen address" disabled={frozen || saving} spellCheck={false} autoComplete="off" onChange={(event) => update("clientHost", event.target.value || undefined)} />
          </FormField>
          <Field>
            <FieldLabel htmlFor="local-client-key">Client key</FieldLabel>
              <Input id="local-client-key" className="mono" type={clientKeyVisible ? "text" : "password"} value={clientKey} readOnly aria-describedby="client-key-note" />
            <div className="flex flex-wrap items-center gap-2">
              <IconButton label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
              <IconButton label="Copy client key" onClick={() => void onCopy("Client key", clientKey)}>{copied === "Client key" ? <Check size={16} /> : <Copy size={16} />}</IconButton>
              <Button type="button" variant="outline" disabled={frozen || saving} onClick={() => void rotateKey()}><RefreshCw size={15} />Rotate key</Button>
            </div>
            <FieldDescription id="client-key-note">{copied === "Client key" ? "Copied" : "Stored in an owner-only file; agent keys are separate."}</FieldDescription>
          </Field>
          <Item>
            <ItemContent><ItemTitle>OpenAI-style endpoint</ItemTitle><ItemDescription>{openAi || "Invalid settings"}</ItemDescription></ItemContent>
            <ItemActions><IconButton label="Copy OpenAI-style endpoint" disabled={!openAi} onClick={() => void onCopy("OpenAI-style endpoint", openAi)}>{copied === "OpenAI-style endpoint" ? <Check /> : <Copy />}</IconButton></ItemActions>
          </Item>
          <Item>
            <ItemContent><ItemTitle>Anthropic-style endpoint</ItemTitle><ItemDescription>{endpoint || "Invalid settings"}</ItemDescription></ItemContent>
            <ItemActions><IconButton label="Copy Anthropic-style endpoint" disabled={!endpoint} onClick={() => void onCopy("Anthropic-style endpoint", endpoint)}>{copied === "Anthropic-style endpoint" ? <Check /> : <Copy />}</IconButton></ItemActions>
          </Item>
          </FieldGroup>
          {isProtected(state) && <p className="sheet-text">Saving briefly restarts protection and updates connected agents. In-flight requests may be interrupted.</p>}
          {(error || externalError) && <p className="sheet-text error" role="alert">{error ?? externalError}</p>}
        </div>
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
      <strong className={mono ? "mono" : undefined} title={value}>{value}</strong>
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
      return { title: "Verification failed", detail: "Nothing was sent. Check the service address and start again.", tone: "danger", settings: "Open Settings" };
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

/** The plain-language outcome of one request. */
function outcomeOf(activity: RequestActivity): { label: string; tone: Tone; icon: typeof ShieldCheck } {
  if (!activity.leftDevice) {
    return { label: "Blocked locally", tone: "neutral", icon: Ban };
  }
  if (activity.verified === false) {
    return { label: "Proof failed", tone: "danger", icon: TriangleAlert };
  }
  if (activity.status < 200 || activity.status >= 300) {
    return { label: "Upstream failed", tone: "danger", icon: TriangleAlert };
  }
  if (activity.verified === true) {
    return { label: "Protected", tone: "success", icon: ShieldCheck };
  }
  if (activity.receiptId) {
    return { label: "Proof pending", tone: "warning", icon: LoaderCircle };
  }
  return { label: "Proof unavailable", tone: "warning", icon: ShieldX };
}

function agentName(id?: string): string {
  switch (id) {
    case "codex": return "Codex";
    case "claude-code": return "Claude Code";
    case "opencode": return "OpenCode";
    case "pi": return "Pi";
    case "hermes": return "Hermes Agent";
    case "local-tools": return "Local API";
    default: return id ?? "Unknown client";
  }
}

function displayAgentName(agent: Pick<AgentStatus, "id" | "name">): string {
  return agent.id === "hermes" ? "Hermes Agent" : agent.name;
}

function sortAgents(agents: AgentStatus[]): AgentStatus[] {
  const order = ["claude-code", "codex", "hermes", "pi", "opencode"];
  return [...agents].sort((left, right) => {
    const leftIndex = order.indexOf(left.id);
    const rightIndex = order.indexOf(right.id);
    return (leftIndex < 0 ? order.length : leftIndex) - (rightIndex < 0 ? order.length : rightIndex);
  });
}

function usageSince(range: string): number | undefined {
  const seconds = range === "24h" ? 86_400 : range === "7d" ? 604_800 : range === "30d" ? 2_592_000 : 0;
  return seconds ? Math.floor(Date.now() / 1000) - seconds : undefined;
}

function rangeLabel(range: string): string {
  switch (range) {
    case "24h": return "Today";
    case "7d": return "Last 7 days";
    case "30d": return "Last 30 days";
    default: return "All time";
  }
}

function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(tokens >= 10_000_000 ? 0 : 1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(tokens >= 100_000 ? 0 : 1)}K`;
  return tokens.toLocaleString();
}

function currency(value: number): string {
  const digits = value > 0 && value < 0.01 ? 4 : 2;
  return new Intl.NumberFormat(undefined, { style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits }).format(value);
}

function maskClientKey(key: string): string {
  if (!key) return "Unavailable";
  const prefix = key.startsWith("sk-pag-") ? "sk-pag-" : key.startsWith("pag_") ? "pag_" : "";
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

function openAiEndpoint(proxyUrl?: string): string | undefined {
  return proxyUrl ? `${proxyUrl.replace(/\/+$/, "")}/v1` : undefined;
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

export function Renderer(): React.JSX.Element {
  // Native child windows and the main window share one component entry point.
  const nativeDialog = query.get("native-dialog");
  return nativeDialog === "profiles" ? <NativeProfilesWindow repair={query.get("repair") === "1"} />
    : nativeDialog === "profile-editor" ? <NativeProfilesWindow repair={false} editor />
    : nativeDialog === "privacy" ? <NativePrivacyWindow />
      : nativeDialog === "local-api" ? <NativeLocalApiWindow />
        : nativeDialog === "usage-proof" ? <NativeUsageProofWindow initialRecordId={query.get("record") ?? ""} />
          : <App />;
}
