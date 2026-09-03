import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  BatteryMedium,
  Ban,
  ChartNoAxesColumn,
  Check,
  ChevronLeft,
  ChevronRight,
  Download,
  Eye,
  EyeOff,
  Laptop,
  LayoutGrid,
  LoaderCircle,
  LockOpen,
  Network,
  RefreshCw,
  Server,
  Settings,
  ShieldCheck,
  ShieldX,
  SquareTerminal,
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

import { desktopApi as liveApi } from "./desktop-api";
import { brand } from "./generated/brand";
import { mockApi } from "./mock-api";
import type {
  AgentPreview,
  AgentStatus,
  DesktopApi,
  GatewayState,
  ModelSummary,
  RequestActivity,
  UsagePage,
  UsageQuery,
  UsageSummary,
  VerificationCheck,
} from "../shared/contracts";
import "./styles.css";

// `?mock=<scenario>` renders the window against canned state for screenshots.
const query = new URLSearchParams(window.location.search);
const previewMode = query.has("mock");
const desktopApi: DesktopApi = previewMode ? mockApi(query.get("mock")) : liveApi;

const INITIAL_STATE: GatewayState = {
  status: "stopped",
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
  config: { remoteUrl: brand.service.defaultUrl, requireProductionOs: false },
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

function BrandAppIcon({ className = "", busy = false }: { className?: string; busy?: boolean }): React.JSX.Element {
  const classes = ["brand-app-icon", className, busy ? "is-busy" : ""].filter(Boolean).join(" ");
  return (
    <picture className={classes} aria-hidden="true">
      <source media="(prefers-color-scheme: dark)" srcSet={brand.appIcon.dark} />
      <img src={brand.appIcon.light} alt="" />
    </picture>
  );
}

type View = "overview" | "agents" | "usage" | "settings";
type SettingsTarget = "confidential" | "privacy" | "local-api";
type UsageMetric = "tokens" | "cost" | "requests";
type Tone = "success" | "warning" | "danger" | "neutral";
const VIEWS: { id: View; label: string; icon: typeof LayoutGrid }[] = [
  { id: "overview", label: "Overview", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: SquareTerminal },
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

/** A connect or disconnect in progress: the sheet's state. */
interface Pending {
  agent: AgentStatus;
  connect: boolean;
  model: string;
  preview?: AgentPreview;
  error?: string;
  loading: boolean;
}

/** Native modal dialog: `showModal()` gives browser-native focus
 * containment, Escape handling, and an inert background. Focus returns to
 * the opener on close. */
function useModalDialog(onClose: () => void): React.RefObject<HTMLDialogElement | null> {
  const ref = useRef<HTMLDialogElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  // Captured during the first render, before the commit that opens the dialog
  // disables the trigger (which would drop browser focus to <body>).
  const [opener] = useState<HTMLElement | null>(() =>
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );
  useEffect(() => {
    const node = ref.current;
    node?.showModal();
    const onCloseEvent = () => closeRef.current();
    node?.addEventListener("close", onCloseEvent);
    return () => {
      node?.removeEventListener("close", onCloseEvent);
      // Deferred: the opener may only be re-enabled by the same commit that
      // unmounts the dialog.
      window.setTimeout(() => opener?.focus(), 0);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return ref;
}

function App(): React.JSX.Element {
  const [view, setView] = useState<View>("overview");
  const [settingsTarget, setSettingsTarget] = useState<SettingsTarget>();
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(INITIAL_STATE.config.remoteUrl);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [savingKey, setSavingKey] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [clientKey, setClientKey] = useState("");
  const [clientKeyVisible, setClientKeyVisible] = useState(false);
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [pending, setPending] = useState<Pending>();
  const [applying, setApplying] = useState(false);
  const [confirmRestoreAll, setConfirmRestoreAll] = useState(false);
  const [notice, setNotice] = useState<{ id: number; text: string }>();
  const [previewTrayOpen, setPreviewTrayOpen] = useState(false);
  const [previewOpenAtLogin, setPreviewOpenAtLogin] = useState(true);
  const copyTimer = useRef<number | undefined>(undefined);
  const busy = state.status === "verifying";
  const running = state.status === "verified" || state.status === "blocked";
  const verified = state.status === "verified";
  const endpointDown = Boolean(state.endpointError);
  const models = state.catalog?.models ?? [];
  const catalogReady = verified && models.length > 0;

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
      (nextState) => active && setState(nextState),
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    void desktopApi.getClientKey().then(
      (key) => active && setClientKey(key),
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    return () => {
      active = false;
      if (copyTimer.current !== undefined) window.clearTimeout(copyTimer.current);
      unsubscribe();
      unsubscribeNavigate();
    };
  }, []);

  // The form mirrors the configuration the backend will start with, so a
  // start from the tray switch shows up here too.
  const configuredUrl = state.config.remoteUrl;
  const configuredPolicy = state.config.requireProductionOs;
  useEffect(() => {
    setRemoteUrl(configuredUrl);
    setRequireProductionOs(configuredPolicy);
  }, [configuredUrl, configuredPolicy]);

  const loadAgents = useCallback(async () => {
    try {
      setAgents(await desktopApi.listAgents());
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }, []);

  // Agent status depends on the verified catalog, so reload with the session.
  const catalogRevision = state.catalog?.revision;
  useEffect(() => {
    void loadAgents();
  }, [loadAgents, catalogRevision, verified]);

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

  const toggleGateway = () =>
    void run(() =>
      running || busy ? desktopApi.stop() : desktopApi.start({ remoteUrl, requireProductionOs }),
    );

  const verifyConfiguration = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!state.apiKeySaved && !apiKeyDraft.trim()) {
      return;
    }
    setSavingKey(true);
    setActionError(undefined);
    try {
      if (apiKeyDraft.trim()) {
        setState(await desktopApi.setApiKey(apiKeyDraft));
        setApiKeyDraft("");
      }
      setState(running ? await desktopApi.refreshCatalog() : await desktopApi.start({ remoteUrl, requireProductionOs }));
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      setSavingKey(false);
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

  // A preview is fetched once the inputs are known: immediately for a
  // disconnect, after the model choice for a connect.
  const loadPreview = async (agent: AgentStatus, connect: boolean, model: string) => {
    setPending({ agent, connect, model, loading: true });
    try {
      const preview = await desktopApi.previewAgent(agent.id, connect, connect ? { defaultModel: model || undefined } : {});
      setPending((current) =>
        current?.agent.id === agent.id ? { ...current, preview, error: undefined, loading: false } : current,
      );
    } catch (error) {
      setPending((current) =>
        current?.agent.id === agent.id
          ? { ...current, preview: undefined, error: errorMessage(error), loading: false }
          : current,
      );
    }
  };

  const openPending = (agent: AgentStatus, connect: boolean) => {
    if (connect && agent.id === "codex") {
      setPending({ agent, connect: true, model: "", loading: false });
    } else if (connect) {
      void loadPreview(agent, true, "");
    } else {
      void loadPreview(agent, false, "");
    }
  };

  const confirmPending = async () => {
    if (!pending?.preview) {
      return;
    }
    const { agent, connect, model, preview } = pending;
    setApplying(true);
    setActionError(undefined);
    try {
      await desktopApi.applyAgent(agent.id, connect, preview.revision, connect ? { defaultModel: model || undefined } : {});
      setPending(undefined);
      await loadAgents();
    } catch (error) {
      setPending((current) => current ? { ...current, error: errorMessage(error) } : current);
    } finally {
      setApplying(false);
    }
  };

  const restoreAll = async () => {
    setApplying(true);
    setActionError(undefined);
    try {
      setAgents(await desktopApi.disconnectAllAgents());
      setConfirmRestoreAll(false);
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
  const locked = Boolean(pending) || applying;
  const focusPageHeading = (next: View) => {
    window.requestAnimationFrame(() => document.getElementById(`page-title-${next}`)?.focus());
  };
  const changeView = (next: View, focusHeading = true) => {
    if (next === "settings") setSettingsTarget(undefined);
    setView(next);
    if (focusHeading) focusPageHeading(next);
  };
  const openSettings = (target: SettingsTarget) => {
    setSettingsTarget(target);
  };

  const windowContent = (
    <main className="app-shell">
      <Sidebar view={view} previewControls={previewMode} onChange={changeView} />
      <section className="workspace">
        <PageHeader
          view={view}
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
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
            catalogReady={catalogReady}
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
            onSelect={openPending}
          />
        )}
        {view === "agents" && (
          <AgentsView
            agents={agents}
            endpointDown={endpointDown}
            catalogReady={catalogReady}
            locked={locked}
            problem={problem}
            onSelect={openPending}
          />
        )}
        {view === "usage" && (
          <UsageView
            state={state}
            agents={agents}
            problem={problem}
            onNotice={(text) => setNotice({ id: Date.now(), text })}
          />
        )}
        {view === "settings" && (
          <SettingsView
            state={state}
            busy={busy}
            running={running}
            requireProductionOs={requireProductionOs}
            copied={copied}
            anyRecorded={anyRecorded}
            locked={locked}
            problem={problem}
            onPolicy={setRequireProductionOs}
            onCopy={copy}
            onRestoreAll={() => setConfirmRestoreAll(true)}
            onSupport={() => void run(() => desktopApi.openSupport())}
            onOpen={openSettings}
          />
        )}
        </div>
      </section>

      {pending && (
        <ConnectSheet
          pending={pending}
          models={models}
          catalogReady={catalogReady}
          applying={applying}
          onModel={(model) => void loadPreview(pending.agent, true, model)}
          onCancel={() => setPending(undefined)}
          onConfirm={() => void confirmPending()}
        />
      )}
      {confirmRestoreAll && (
        <RestoreAllSheet
          applying={applying}
          error={actionError}
          onCancel={() => setConfirmRestoreAll(false)}
          onConfirm={() => void restoreAll()}
        />
      )}
      {settingsTarget === "confidential" && (
        <ServiceConfigurationSheet
          state={state}
          busy={busy}
          running={running}
          verified={verified}
          remoteUrl={remoteUrl}
          apiKeyDraft={apiKeyDraft}
          savingKey={savingKey}
          onRemoteUrl={setRemoteUrl}
          onApiKeyDraft={setApiKeyDraft}
          onVerify={verifyConfiguration}
          onClearKey={() => void run(() => desktopApi.clearApiKey())}
          onClose={() => setSettingsTarget(undefined)}
        />
      )}
      {settingsTarget === "privacy" && (
        <PrivacyVerificationSheet state={state} verified={verified} onClose={() => setSettingsTarget(undefined)} />
      )}
      {settingsTarget === "local-api" && (
        <LocalApiSheet
          state={state}
          clientKey={clientKey}
          clientKeyVisible={clientKeyVisible}
          copied={copied}
          onCopy={copy}
          onToggleKey={() => setClientKeyVisible((visible) => !visible)}
          onRotate={rotateClientKey}
          onClose={() => setSettingsTarget(undefined)}
        />
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
      <MacMenuBar trayOpen={previewTrayOpen} onTray={() => setPreviewTrayOpen((open) => !open)} />
      <div className="desktop-window">{windowContent}</div>
      {previewTrayOpen && (
        <PreviewTrayMenu
          state={state}
          busy={busy}
          running={running}
          endpointDown={endpointDown}
          openAtLogin={previewOpenAtLogin}
          onProtection={toggleGateway}
          onOpen={() => setPreviewTrayOpen(false)}
          onSettings={() => {
            setPreviewTrayOpen(false);
            changeView("settings");
          }}
          onOpenAtLogin={() => setPreviewOpenAtLogin((enabled) => !enabled)}
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
}: {
  view: View;
  previewControls: boolean;
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
        <BrandAppIcon className="brand-mark" />
        <span>{brand.productName}</span>
      </div>
      <nav aria-label="Main navigation" onKeyDown={onKeyDown}>
        {VIEWS.map((entry) => {
          const Icon = entry.icon;
          return (
            <button
              key={entry.id}
              id={`nav-${entry.id}`}
              className="nav-item"
              aria-label={entry.label}
              aria-current={view === entry.id ? "page" : undefined}
              tabIndex={view === entry.id ? 0 : -1}
              onClick={() => onChange(entry.id, true)}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{entry.label}</span>
            </button>
          );
        })}
      </nav>
    </aside>
  );
}

function MacMenuBar({ trayOpen, onTray }: { trayOpen: boolean; onTray(): void }): React.JSX.Element {
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
        <button className={`tray-trigger${trayOpen ? " is-open" : ""}`} aria-label="Private AI Gateway menu" aria-expanded={trayOpen} onClick={onTray}>
          <span className="tray-template-icon" aria-hidden="true" />
        </button>
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
  openAtLogin: boolean;
  onProtection(): void;
  onOpen(): void;
  onSettings(): void;
  onOpenAtLogin(): void;
  onQuit(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  return (
    <div className="preview-tray" role="menu" aria-label="Private AI Gateway">
      <div className="preview-tray-heading">
        <BrandAppIcon />
        <span><strong>{brand.productName}</strong><small>{serviceHost(brand.service.defaultUrl)}</small></span>
      </div>
      <div className="preview-tray-protection">
        <span><strong>Protected</strong><small>{running ? "On" : busy ? "Starting" : "Off"}</small></span>
        <button
          className="switch"
          role="switch"
          aria-label={running || busy ? "Stop protection" : "Start protection"}
          aria-checked={running}
          disabled={busy || endpointDown}
          onClick={onProtection}
        ><span /></button>
      </div>
      <div className="preview-tray-separator" />
      <button className="preview-tray-item" role="menuitem" onClick={onOpen}>Open {brand.productName}</button>
      <button className="preview-tray-item" role="menuitem" onClick={onSettings}>Settings…</button>
      <div className="preview-tray-separator" />
      <button className="preview-tray-item" role="menuitemcheckbox" aria-checked={openAtLogin} onClick={onOpenAtLogin}>
        <span className="preview-tray-check" aria-hidden="true">{openAtLogin ? "✓" : ""}</span>
        Open at Login
      </button>
      <button className="preview-tray-item" role="menuitem" onClick={onQuit}>Quit {brand.productName}</button>
      <span className="sr-only" role="status">{verdict.title}</span>
    </div>
  );
}

function PageHeader({
  view,
  state,
  busy,
  running,
  endpointDown,
  onToggle,
}: {
  view: View;
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const title = VIEWS.find((entry) => entry.id === view)?.label ?? "";
  const verdict = presentation(state);
  return (
    <header className="page-header" data-tauri-drag-region>
      <h1 id={`page-title-${view}`} tabIndex={-1}>{title}</h1>
      {view !== "overview" && (
        <div className="page-protection">
          <span className="page-switch-copy">
            <strong>Protected</strong>
            <small className={verdict.tone === "success" ? "is-on" : verdict.tone === "danger" ? "is-error" : undefined}>{busy ? "Starting" : running ? "On" : "Off"}</small>
          </span>
          <ProtectedControl
            state={state}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
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
  catalogReady,
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
}: {
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  catalogReady: boolean;
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
}): React.JSX.Element {
  const recent = state.activity.slice(0, 5);
  return (
    <div className="overview-page">
      <StatusSurface
        state={state}
        agents={agents}
        busy={busy}
        running={running}
        endpointDown={endpointDown}
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
        <OverviewModule title="Local API">
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
        <OverviewModule title="Session usage" meta="This session">
          <SessionSummary summary={state.sessionUsage} running={running} />
        </OverviewModule>
        <OverviewModule title="Agents" action="View all" onAction={onAgents}>
          <div className="preview-list">
            {agents.length === 0 && <EmptyState text="Agent configs unavailable" />}
            {sortAgents(agents).slice(0, 5).map((agent) => (
              <AgentRow
                key={agent.id}
                agent={agent}
                compact
                disabled={locked}
                connectBlocked={endpointDown || !catalogReady}
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
              <UsagePreviewRow key={item.id} activity={item} />
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
  onToggle,
  onSettings,
  onPrivacy,
}: {
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  onToggle(): void;
  onSettings(): void;
  onPrivacy(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const connected = agents.filter((agent) => agent.connected).length;
  const enabled = agents.filter((agent) => agent.recorded).length;
  const host = serviceHost(state.remoteUrl ?? state.config.remoteUrl);
  return (
    <section className={`status-surface status-${state.status} ${state.status === "verified" && state.apiKeySaved ? "status-ready" : ""}`} aria-label="Protection status">
      <TrackLayer side="left" lines={PLAINTEXT_TRACKS} active={state.status === "verified" && state.apiKeySaved} />
      <TrackLayer side="right" lines={TLS_TRACKS} active={state.status === "verified" && state.apiKeySaved} />
      <div className="status-glow" aria-hidden="true" />
      <div className="status-edge status-edge-left" aria-hidden="true" />
      <div className="status-edge status-edge-right" aria-hidden="true" />

      <div className="status-segment status-local">
        <div className="status-heading"><Laptop size={18} aria-hidden="true" /><span>This Mac</span></div>
        <span className="status-meta">{enabled} enabled · {connected} active</span>
        <div className="status-agent-icons" role="group" aria-label="Enabled agents">
          {agents.filter((agent) => agent.recorded).slice(0, 5).map((agent) => (
            <span className="status-agent-icon" key={agent.id} title={agent.name}>
              {AGENT_ICONS[agent.id] ? <img src={AGENT_ICONS[agent.id]} alt={agent.name} /> : agent.name.slice(0, 1)}
            </span>
          ))}
        </div>
        <p>Enabled agents send their requests to the gateway on this Mac.</p>
      </div>

      <div className="status-segment status-gateway">
        <div className="gateway-core">
          <BrandAppIcon className="gateway-mark" busy={busy} />
          <strong>{brand.productName}</strong>
          <span className={`gateway-verdict state-${verdict.tone}`} aria-live="polite">{verdict.title}</span>
          <ProtectedControl
            state={state}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            onToggle={onToggle}
            iconOnly
          />
        </div>
      </div>

      <div className="status-segment status-remote">
        <div className="status-heading"><span>Confidential AI</span><Server size={18} aria-hidden="true" /></div>
        <code className="status-host">{host}</code>
        {state.status === "verified" ? (
          <div className="status-facts">
            <span>Verified hardware <Check size={12} aria-hidden="true" /></span>
            <span>{state.sessionUsage.protected.toLocaleString()} answers this session <Check size={12} aria-hidden="true" /></span>
          </div>
        ) : (
          <div className="status-facts status-facts-off">
            <span>Not verified <span className="dot" aria-hidden="true" /></span>
            <span>No answers this session <span className="dot" aria-hidden="true" /></span>
          </div>
        )}
        <div className="status-actions">
          <IconButton label="Confidential AI settings" onClick={onSettings}><Settings size={16} /></IconButton>
          <IconButton label="Privacy verification" onClick={onPrivacy}><ShieldCheck size={16} /></IconButton>
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
  compact = false,
  iconOnly = false,
  onToggle,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  compact?: boolean;
  iconOnly?: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const checked = running || busy;
  const label = busy ? "Cancel protection start" : running ? "Stop protection" : "Start protection";
  return (
    <div className={`protected-control ${compact ? "is-compact" : ""} ${iconOnly && !compact ? "is-icon-only" : ""}`}>
      {!iconOnly && <span>Protected</span>}
      <button
        type="button"
        className="switch"
        role="switch"
        aria-checked={checked}
        aria-label={label}
        disabled={endpointDown && !checked}
        title={endpointDown && !checked ? state.endpointError : label}
        onClick={onToggle}
      >
        <span />
      </button>
    </div>
  );
}

function OverviewModule({
  title,
  meta,
  action,
  onAction,
  children,
}: React.PropsWithChildren<{
  title: string;
  meta?: string;
  action?: string;
  onAction?(): void;
}>): React.JSX.Element {
  return (
    <section className="overview-module">
      <header className="overview-module-title">
        <h2>{title}</h2>
        {meta && <span>{meta}</span>}
        {action && onAction && <button className="module-action" onClick={onAction}>{action}</button>}
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
        <button
          className="copy-surface"
          disabled={!proxyUrl}
          aria-label={`${endpointLabel}: ${proxyUrl ?? "Unavailable"}. Copy`}
          onClick={() => proxyUrl && void onCopy(endpointLabel, proxyUrl)}
        >
          <span className="row-title-line">
            <span className="row-title">Endpoint</span>
            <span className="row-side">{proxyUrl ? "Available" : "Stopped"}</span>
          </span>
          <code className="row-note">{proxyUrl ?? "Unavailable"}</code>
          <span className={`copy-feedback ${copied === endpointLabel ? "is-copied" : ""}`}>{copied === endpointLabel ? "Copied" : "Copy"}</span>
        </button>
        <IconButton className="row-action" label="Local API settings" onClick={onSettings}><Settings size={16} /></IconButton>
      </div>
      <div className="copy-row">
        <button className="copy-surface" disabled={!clientKey} aria-label={`${keyLabel}: ${clientKeyVisible ? clientKey : "hidden"}. Copy`} onClick={() => clientKey && void onCopy(keyLabel, clientKey)}>
          <span className="row-title-line">
            <span className="row-title">Client key</span>
            <span className="row-side">for your own tools</span>
          </span>
          <code className="row-note">{clientKey ? clientKeyVisible ? clientKey : maskClientKey(clientKey) : "Unavailable"}</code>
          <span className={`copy-feedback ${copied === keyLabel ? "is-copied" : ""}`}>{copied === keyLabel ? "Copied" : "Copy"}</span>
        </button>
        <IconButton className="row-action" label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
      </div>
      {endpointError && <p className="inline-error">{endpointError}</p>}
    </div>
  );
}

function SessionSummary({ summary, running }: { summary: UsageSummary; running: boolean }): React.JSX.Element {
  const forwarded = Math.max(0, summary.requests - summary.blockedLocally);
  const totalTokens = summary.inputTokens + summary.outputTokens;
  const protectedRate = forwarded ? Math.round((summary.protected / forwarded) * 100) : 0;
  return (
    <div className="session-summary">
      <div><span>Requests</span><strong>{summary.requests.toLocaleString()}</strong><small>{summary.blockedLocally.toLocaleString()} blocked locally</small></div>
      <div><span>Tokens</span><strong>{formatTokens(totalTokens)}</strong><small>{formatTokens(summary.inputTokens)} in · {formatTokens(summary.outputTokens)} out</small></div>
      <div><span>Cost</span><strong>{currency(summary.costUsd)}</strong><small>Estimated from model prices</small></div>
      <div><span>Protected</span><strong>{forwarded ? `${protectedRate}%` : "—"}</strong><small>{running ? `${summary.protected} of ${forwarded} answers` : `${summary.protected} of ${forwarded} answers`}</small></div>
    </div>
  );
}

function UsagePreviewRow({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  return (
    <div className="preview-row">
      <span className={`preview-dot state-${outcome.tone}`} aria-hidden="true" />
      <span><strong>{agentName(activity.agent)}</strong><code>{activity.path}</code></span>
      <time>{formatTimestamp(activity.at * 1_000, true)}</time>
    </div>
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

function IconButton({
  label,
  className = "",
  disabled = false,
  onClick,
  children,
}: React.PropsWithChildren<{ label: string; className?: string; disabled?: boolean; onClick(): void }>): React.JSX.Element {
  return <button type="button" className={`icon-button ${className}`} aria-label={label} title={label} disabled={disabled} onClick={onClick}>{children}</button>;
}

function AgentsView({
  agents,
  endpointDown,
  catalogReady,
  locked,
  problem,
  onSelect,
}: {
  agents: AgentStatus[];
  endpointDown: boolean;
  catalogReady: boolean;
  locked: boolean;
  problem?: string;
  onSelect(agent: AgentStatus, connect: boolean): void;
}): React.JSX.Element {
  const connected = agents.filter((agent) => agent.connected).length;
  const enabled = agents.filter((agent) => agent.recorded).length;
  return (
    <div className="page-body">
      {problem && <p className="banner" role="alert">{problem}</p>}
      <p className="page-intro">Enabled agents use {brand.productName} whenever protection is on. Their previous settings return when you disconnect them.</p>
      <section className="group" aria-labelledby="agents-title">
        <h2 className="group-title" id="agents-title">Configured agents <span>{enabled} enabled · {connected} active</span></h2>
        <div className="inset">
          {agents.length === 0 && <EmptyState text="Agent configs unavailable" />}
          {sortAgents(agents).map((agent) => (
            <AgentRow
              key={agent.id}
              agent={agent}
              disabled={locked}
              connectBlocked={endpointDown || !catalogReady}
              onSelect={(connect) => onSelect(agent, connect)}
            />
          ))}
        </div>
      </section>
      <p className="page-footnote">Available models sync automatically from the verified service.</p>
    </div>
  );
}

function AgentRow({
  agent,
  disabled,
  connectBlocked,
  compact = false,
  onSelect,
}: {
  agent: AgentStatus;
  disabled: boolean;
  /** Connecting needs the verified catalog and a bound endpoint; disconnecting never does. */
  connectBlocked: boolean;
  compact?: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const name = displayAgentName(agent);
  const presence = agent.attention
    ? { label: "Needs attention", tone: "warning" as Tone, icon: TriangleAlert }
    : agent.error
      ? { label: "Error", tone: "danger" as Tone, icon: TriangleAlert }
      : agent.connected
        ? { label: "Connected", tone: "success" as Tone, icon: ShieldCheck }
        : agent.installed
          ? { label: "Not connected", tone: "neutral" as Tone, icon: undefined }
          : { label: "CLI not found", tone: "neutral" as Tone, icon: undefined };
  const disconnecting = agent.recorded;
  const actionable = disconnecting || (!connectBlocked && !agent.error);
  const note = agent.attention ?? agent.error;
  return (
    <div className={`agent-block row ${compact ? "agent-compact" : ""}`} title={agent.configPath}>
      <span className={agent.connected ? "agent-mark-on" : undefined}><AgentMark agent={agent} /></span>
      <div className="row-main">
        <span className="row-title-line">
          <span className="row-title">{name}</span>
          <StateLabel tone={presence.tone} icon={presence.icon} text={presence.label} />
        </span>
        <code className="row-note agent-config" title={agent.configPath}>{homePath(agent.configPath)}</code>
        {note && <p className="row-note">{note}</p>}
      </div>
      <label className="inline-switch" title={!disconnecting && connectBlocked ? "Start protection first; models come from the verified service" : undefined}>
        <input
          type="checkbox"
          checked={disconnecting}
          disabled={disabled || !actionable}
          aria-label={`${disconnecting ? "Disconnect" : "Connect"} ${name}`}
          onChange={() => onSelect(!disconnecting)}
        />
        <span aria-hidden="true"><span /></span>
      </label>
    </div>
  );
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
    <span className={`state state-${tone}`}>
      {Icon ? <Icon size={13} aria-hidden="true" /> : <span className="dot" aria-hidden="true" />}
      {text}
    </span>
  );
}

function UsageView({
  state,
  agents,
  problem,
  onNotice,
}: {
  state: GatewayState;
  agents: AgentStatus[];
  problem?: string;
  onNotice(text: string): void;
}): React.JSX.Element {
  const [agent, setAgent] = useState("");
  const [model, setModel] = useState("");
  const [range, setRange] = useState("7d");
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const [page, setPage] = useState<UsagePage>();
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const [confirmClear, setConfirmClear] = useState(false);
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
    setSelected(undefined);
  };
  const current = page?.items.find((item) => item.id === selected);
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
      const count = await desktopApi.clearUsage();
      setConfirmClear(false);
      resetPagination();
      setPage(undefined);
      onNotice(`Deleted ${count.toLocaleString()} usage ${count === 1 ? "record" : "records"}`);
    } catch (clearError) {
      setError(errorMessage(clearError));
    }
  };

  return (
    <div className="usage-page">
      {(problem || error) && <p className="banner" role="alert">{problem ?? error}</p>}
      <div className="usage-toolbar" role="group" aria-label="Usage filters">
        <label>Agent<select value={agent} onChange={(event) => { setAgent(event.target.value); resetPagination(); }}><option value="">All agents</option>{agentOptions.map((entry) => <option key={entry} value={entry}>{agentName(entry)}</option>)}</select></label>
        <label>Model<select value={model} onChange={(event) => { setModel(event.target.value); resetPagination(); }}><option value="">All models</option>{page?.models.map((entry) => <option key={entry} value={entry}>{entry}</option>)}</select></label>
        <fieldset className="filter-field time-filter">
          <legend>Time</legend>
          <div className="segmented-control" role="group" aria-label="Usage time range">
            {(["24h", "7d", "30d", "all"] as const).map((value) => (
              <button
                type="button"
                key={value}
                aria-pressed={range === value}
                onClick={() => { setRange(value); resetPagination(); }}
              >
                {{ "24h": "Today", "7d": "7 days", "30d": "30 days", all: "All" }[value]}
              </button>
            ))}
          </div>
        </fieldset>
      </div>
      <UsageStats page={page} />
      <section className="group usage-over-time" aria-labelledby="usage-chart-title">
        <h2 className="group-title" id="usage-chart-title">Usage over time <span>{rangeLabel(range)}</span></h2>
        <UsageChart page={page} metric={metric} onMetric={setMetric} />
      </section>
      <section className="group usage-history" aria-labelledby="usage-history-title">
        <h2 className="group-title" id="usage-history-title" tabIndex={-1}>
          Usage history
          <span aria-live="polite">{loading ? "Loading" : `${page?.summary.requests ?? 0} records · kept on this Mac`}</span>
          <span className="group-actions">
            <IconButton label="Export usage as CSV" onClick={() => void exportCsv()}><Download size={16} /></IconButton>
            <IconButton label="Clear usage history" onClick={() => setConfirmClear(true)}><Trash2 size={16} /></IconButton>
          </span>
        </h2>
        <div className="inset list" aria-busy={loading}>
          {loading && !page && <EmptyState text="Loading usage history…" />}
          {!loading && page?.items.length === 0 && <EmptyState text={page.summary.requests === 0 ? "No saved usage matches these filters." : "No records on this page."} />}
          <ul className="list-items" aria-label="Usage history">
            {page?.items.map((item) => {
              const outcome = outcomeOf(item);
              return (
                <li key={item.id}>
                  <button className="row list-row usage-row" aria-pressed={selected === item.id} onClick={() => setSelected((value) => value === item.id ? undefined : item.id)}>
                    <span className="row-main"><span className="row-title">{agentName(item.agent)}</span><StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} /><code className="row-note">{item.model ?? item.path}</code></span>
                    <span className="usage-amount"><strong>{formatTokens((item.inputTokens ?? 0) + (item.outputTokens ?? 0))}</strong><small>tokens</small></span>
                    <span className="usage-amount"><strong>{item.costUsd === undefined ? "—" : currency(item.costUsd)}</strong><small>cost</small></span>
                    <time className="row-side">{formatTimestamp(item.at * 1_000, true)}</time>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
        <div className="pagination">
          <IconButton
            label="Previous usage page"
            disabled={loading || cursors.length === 1}
            onClick={() => {
              focusAfterPage.current = true;
              setSelected(undefined);
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
              setSelected(undefined);
              setCursors((value) => [...value, page.nextCursor]);
            }}
          ><ChevronRight size={16} /></IconButton>
        </div>
      </section>
      <aside className="inspector" aria-live="polite" aria-label="Usage details">
        {current ? <Evidence activity={current} /> : <p className="inspector-hint">Select a request to inspect usage and proof details.</p>}
      </aside>
      {confirmClear && <ClearUsageSheet onCancel={() => setConfirmClear(false)} onConfirm={() => void clear()} />}
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
  metric,
  onMetric,
}: {
  page?: UsagePage;
  metric: UsageMetric;
  onMetric(metric: UsageMetric): void;
}): React.JSX.Element {
  const series = page?.series.slice(-30) ?? [];
  const value = (point: UsagePage["series"][number]) => metric === "tokens" ? point.tokens : metric === "cost" ? point.costUsd : point.requests;
  const peak = Math.max(1, ...series.map(value));
  const labelIndexes = chartLabelIndexes(series.length);
  return (
    <figure className="usage-chart" aria-label={`${metric} usage by day`}>
      <div className="chart-toolbar">
        <div className="segmented-control chart-metric" role="group" aria-label="Chart metric">
          {(["tokens", "cost", "requests"] as const).map((entry) => (
            <button type="button" key={entry} aria-pressed={metric === entry} onClick={() => onMetric(entry)}>
              {{ tokens: "Tokens", cost: "Cost", requests: "Requests" }[entry]}
            </button>
          ))}
        </div>
        {metric === "tokens" && <span className="chart-legend"><i className="input" />Input <i className="output" />Output</span>}
      </div>
      <div className="chart-bars" aria-hidden="true">
        {series.map((point, index) => (
          <div key={point.day} className="chart-column" title={`${point.day}: ${point.tokens.toLocaleString()} tokens, ${point.requests} requests, ${currency(point.costUsd)}`}>
            <span className="chart-stack" style={{ height: `${Math.max(3, value(point) / peak * 100)}%` }}>
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

function chartLabelIndexes(length: number): Set<number> {
  if (length <= 4) {
    return new Set(Array.from({ length }, (_, index) => index));
  }
  return new Set(Array.from({ length: 4 }, (_, index) => Math.round(index * (length - 1) / 3)));
}

function ClearUsageSheet({ onCancel, onConfirm }: { onCancel(): void; onConfirm(): void }): React.JSX.Element {
  const dialog = useModalDialog(onCancel);
  return <dialog ref={dialog} className="sheet" aria-label="Clear usage history"><div className="sheet-heading"><h2>Clear usage history?</h2></div><p className="sheet-text">This permanently deletes the local usage database records. It does not affect provider billing or remote receipt retention.</p><div className="sheet-actions"><button className="button" onClick={onCancel}>Cancel</button><button className="button destructive" onClick={onConfirm}>Clear History</button></div></dialog>;
}

function Evidence({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
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
    <dl className="evidence">
      <dt>Request</dt>
      <dd>
        {agentName(activity.agent)} <code>{activity.method} {activity.path}</code>
      </dd>
      {activity.model && <><dt>Model</dt><dd><code>{activity.model}</code></dd></>}
      <dt>Outcome</dt>
      <dd>
        <StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} />
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
          <dt>Proof</dt>
          <dd>
            {activity.verified === true
              ? "Signed receipt verified: request and response bytes match what this app sent and received."
              : activity.verified === false
                ? "Signed receipt did not verify; treat this response as unprotected."
                : "Signed receipt is present and has not finished verification."}
            <code>{activity.receiptId}</code>
          </dd>
        </>
      )}
      {notes.length > 0 && (
        <>
          <dt>Notes</dt>
          <dd>{notes.join(" ")}</dd>
        </>
      )}
    </dl>
  );
}

function SettingsView({
  state,
  busy,
  running,
  requireProductionOs,
  copied,
  anyRecorded,
  locked,
  problem,
  onPolicy,
  onCopy,
  onRestoreAll,
  onSupport,
  onOpen,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  requireProductionOs: boolean;
  copied?: string;
  anyRecorded: boolean;
  locked: boolean;
  problem?: string;
  onPolicy(value: boolean): void;
  onCopy(label: string, value: string): Promise<void>;
  onRestoreAll(): void;
  onSupport(): void;
  onOpen(target: SettingsTarget): void;
}): React.JSX.Element {
  const frozen = busy || running;
  return (
    <div className="page-body settings-page">
      {problem && <p className="banner" role="alert">{problem}</p>}

      <section className="group" aria-labelledby="service-settings-title">
        <h2 className="group-title" id="service-settings-title">Service</h2>
        <div className="inset">
          <div className="row">
            <span className="row-main">
              <span className="row-title">Confidential AI</span>
              <span className="row-note">{serviceHost(state.remoteUrl ?? state.config.remoteUrl)} · {state.status === "verified" ? "Verified" : "Verification required"}</span>
            </span>
            <button type="button" className="button" onClick={() => onOpen("confidential")}><Settings size={15} />Configure…</button>
          </div>
        </div>
      </section>

      <section className="group" aria-labelledby="local-api-title">
        <h2 className="group-title" id="local-api-title">Local API</h2>
        <div className="inset">
          {state.endpointError && <p className="row-warning">{state.endpointError}</p>}
          <EndpointRow label="Endpoint" value={state.proxyUrl} copied={copied} onCopy={onCopy} />
          <div className="row"><span className="row-main"><span className="row-title">Status</span><StateLabel tone={state.proxyUrl ? "success" : "neutral"} icon={state.proxyUrl ? Check : undefined} text={state.proxyUrl ? "Available" : "Stopped"} /></span></div>
          <div className="row">
            <span className="row-main"><span className="row-title">Listen address, port, client host</span><span className="row-note">Only apps on this Mac can connect unless network access is allowed.</span></span>
            <button type="button" className="button" onClick={() => onOpen("local-api")}><Settings size={15} />Local API settings…</button>
          </div>
        </div>
      </section>

      <section className="group" aria-labelledby="advanced-title">
        <h2 className="group-title" id="advanced-title">Advanced</h2>
        <div className="inset">
          <label className="row toggle-row">
            <span className="row-main"><span className="row-title">Require production OS</span><span className="row-note">Only accept the service when it runs a production OS image.{frozen ? " Stop protection to change this setting." : ""}</span></span>
            <input type="checkbox" checked={requireProductionOs} onChange={(event) => onPolicy(event.target.checked)} disabled={frozen} />
            <span className="toggle-track" aria-hidden="true"><span /></span>
          </label>
        </div>
      </section>

      {anyRecorded && <section className="group" aria-labelledby="agents-settings-title"><h2 className="group-title" id="agents-settings-title">Agents</h2><div className="inset"><div className="row"><span className="row-main"><span className="row-title">Restore all agent configs</span><span className="row-note">Turns every agent off and puts every config back, even while protection is off.</span></span><button className="button" disabled={locked} onClick={onRestoreAll}>Restore all</button></div></div></section>}

      <section className="group" aria-labelledby="support-title"><h2 className="group-title" id="support-title">Support</h2><div className="inset"><div className="row"><span className="row-main"><span className="row-title">{brand.productName}</span><span className="row-note">Version 0.1.0</span></span><button className="button" onClick={onSupport}>Get Help…</button></div></div></section>
    </div>
  );
}

function ServiceConfigurationSheet({
  state,
  busy,
  running,
  verified,
  remoteUrl,
  apiKeyDraft,
  savingKey,
  onRemoteUrl,
  onApiKeyDraft,
  onVerify,
  onClearKey,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  verified: boolean;
  remoteUrl: string;
  apiKeyDraft: string;
  savingKey: boolean;
  onRemoteUrl(value: string): void;
  onApiKeyDraft(value: string): void;
  onVerify(event: React.FormEvent): Promise<void>;
  onClearKey(): void;
  onClose(): void;
}): React.JSX.Element {
  const dialog = useModalDialog(onClose);
  const frozen = busy || running;
  const models = state.catalog?.models ?? [];
  return (
    <dialog ref={dialog} className="sheet service-sheet" aria-label="Confidential AI settings">
      <div className="sheet-heading"><h2>Confidential AI settings</h2></div>
      <form onSubmit={(event) => void onVerify(event)}>
        <div className="sheet-card">
          <label className="sheet-field"><span>Service endpoint</span><input value={remoteUrl} onChange={(event) => onRemoteUrl(event.target.value)} disabled={frozen} spellCheck={false} /></label>
          <label className="sheet-field key-field">
            <span>{brand.service.keyLabel}{state.apiKeySaved && <button type="button" className="link" onClick={onClearKey} disabled={savingKey || frozen}>Delete</button>}</span>
            <input type="password" value={apiKeyDraft} onChange={(event) => onApiKeyDraft(event.target.value)} placeholder={state.apiKeySaved ? "Replace the saved key" : `Paste your ${brand.service.keyLabel}`} disabled={frozen} autoComplete="off" spellCheck={false} aria-label={brand.service.keyLabel} />
            <small>{state.apiKeySaved ? "Using the saved key. Enter a new one to replace it after verification." : "The key is stored in the system credential store and never written into agent configs."}</small>
          </label>
        </div>
        <div className="sheet-card verification-card">
          <div className="verification-heading"><span><strong>{verified ? "Configuration verified" : busy ? "Verifying configuration" : "Configuration not verified"}</strong><small>{verified ? `${models.length} models available` : "Start protection to verify the service and discover models"}</small></span><StateLabel tone={verified ? "success" : busy ? "neutral" : "warning"} icon={verified ? Check : busy ? LoaderCircle : undefined} text={verified ? "Ready" : busy ? "Verifying" : "Not verified"} /></div>
          {models.length > 0 && (
            <div className="model-list service-model-list" role="region" aria-label="Verified model catalog" tabIndex={0}>
              <div className="model-catalog-head">
                <span className="model-catalog-title"><strong>Model catalog</strong><small>{models.length} models · scroll for more</small></span>
                <span>Input / output per 1M</span>
              </div>
              {models.map((model) => <ModelRow key={model.id} model={model} />)}
            </div>
          )}
        </div>
        <div className="sheet-actions">
          <button type="button" className="button" onClick={onClose}>Done</button>
          <button type="submit" className="button primary" disabled={savingKey || busy || (!state.apiKeySaved && !apiKeyDraft.trim())}>{savingKey || busy ? "Verifying…" : verified ? "Verify again" : "Verify"}</button>
        </div>
      </form>
    </dialog>
  );
}

function PrivacyVerificationSheet({ state, verified, onClose }: { state: GatewayState; verified: boolean; onClose(): void }): React.JSX.Element {
  const dialog = useModalDialog(onClose);
  return <dialog ref={dialog} className="sheet privacy-sheet" aria-label="Privacy verification"><div className="sheet-heading"><h2>Privacy verification</h2></div><PrivacyVerification state={state} verified={verified} /><div className="sheet-actions"><button className="button primary" onClick={onClose}>Done</button></div></dialog>;
}

function LocalApiSheet({
  state,
  clientKey,
  clientKeyVisible,
  copied,
  onCopy,
  onToggleKey,
  onRotate,
  onClose,
}: {
  state: GatewayState;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  onCopy(label: string, value: string): Promise<void>;
  onToggleKey(): void;
  onRotate(): Promise<void>;
  onClose(): void;
}): React.JSX.Element {
  const dialog = useModalDialog(onClose);
  return (
    <dialog ref={dialog} className="sheet" aria-label="Local API settings">
      <div className="sheet-heading"><h2>Local API settings</h2></div>
      <div className="sheet-card local-api-sheet-card">
        <EndpointRow label="OpenAI-style endpoint" value={openAiEndpoint(state.proxyUrl)} copied={copied} onCopy={onCopy} />
        <EndpointRow label="Anthropic-style endpoint" value={state.proxyUrl} copied={copied} onCopy={onCopy} />
        <div className="row">
          <span className="row-main"><span className="row-title">Client key</span><code className="row-note">{clientKeyVisible ? clientKey : maskClientKey(clientKey)}</code></span>
          <IconButton label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
        </div>
        <p className="row-footnote">The listener is bound to loopback. This key is stored in an owner-only file and can call every supported local API format.</p>
      </div>
      <div className="sheet-actions"><button className="button destructive" onClick={() => void onRotate()}>Replace Key…</button><button className="button primary" onClick={onClose}>Done</button></div>
    </dialog>
  );
}

/** The three facts behind "Protected", each shown only when it holds now. */
function PrivacyVerification({ state, verified }: { state: GatewayState; verified: boolean }): React.JSX.Element {
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
        : "Not established while protection is off.",
    },
    {
      ok: verified && identity?.trustLevel === "hardware_verified",
      title: "Confidential service verified",
      detail: identity
        ? `Hardware attestation checked: ${hardwareName(identity.teeType)}, ${trustName(identity.trustLevel).toLowerCase()}, built from source ${identity.source.repoCommit ? shorten(identity.source.repoCommit, 11) : "(unknown)"}.`
        : "Not established while protection is off.",
    },
    {
      ok: provenProofs > 0 && failedProofs === 0,
      title: "Response proof verified",
      detail: proofs.length
        ? `${provenProofs} of ${proofs.length} recent answers came with a signed receipt this app verified${failedProofs ? `; ${failedProofs} failed` : ""}.`
        : "Each answer from the service carries a signed receipt this app checks; none checked yet.",
    },
  ];
  return (
    <section className="group" aria-label="Privacy">
      <h2 className="group-title" id="privacy-title" tabIndex={-1}>
        Privacy
        {checks.length > 0 && <span>{checkCount(checks)} checks passed</span>}
      </h2>
      <div className="inset">
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
        {identity && (
          <details className="disclosure">
            <summary>Technical details</summary>
            <div className="identity-grid">
              <Detail label="Hardware" value={hardwareName(identity.teeType)} />
              <Detail label="Trust" value={trustName(identity.trustLevel)} />
              <Detail label="Source" value={identity.source.repoCommit ? shorten(identity.source.repoCommit, 11) : "Unknown"} mono />
              <Detail label="Valid until" value={formatTimestamp(identity.keysetNotAfter * 1_000, true)} />
              <Detail label="Serving" value={identity.serving} />
              <Detail label="Channel" value={passed("id-6") ? "SPKI-pinned attested TLS" : "Not established"} />
              <Detail label="Keyset digest" value={identity.keysetDigest} mono wide />
              {identity.tlsSpki && <Detail label="TLS key" value={identity.tlsSpki} mono wide />}
            </div>
            {checks.map((check) => <CheckRow key={check.id} check={check} />)}
          </details>
        )}
      </div>
    </section>
  );
}

function EndpointRow({
  label,
  value,
  copied,
  onCopy,
}: {
  label: string;
  value?: string;
  copied?: string;
  onCopy(label: string, value: string): Promise<void>;
}): React.JSX.Element {
  return (
    <button
      className="settings-copy-row"
      disabled={!value}
      onClick={() => value && void onCopy(label, value)}
      aria-label={`${label}: ${value ?? "Unavailable"}. Copy`}
    >
      <span className="row-main">
        <span className="row-title">{label}</span>
        <code className="row-note" title={value}>{value ?? "Unavailable"}</code>
      </span>
      <span className={`copy-feedback-inline ${copied === label ? "is-copied" : ""}`}>{copied === label ? "Copied" : "Copy"}</span>
    </button>
  );
}

function ModelRow({ model }: { model: ModelSummary }): React.JSX.Element {
  const capabilities = [...model.inputModalities, ...model.capabilities];
  const prices = [
    model.inputPricePerMillion === undefined ? undefined : `${formatPrice(model.inputPricePerMillion)} input`,
    model.outputPricePerMillion === undefined ? undefined : `${formatPrice(model.outputPricePerMillion)} output`,
    model.cacheReadPricePerMillion === undefined ? undefined : `${formatPrice(model.cacheReadPricePerMillion)} cache read`,
    model.cacheWritePricePerMillion === undefined ? undefined : `${formatPrice(model.cacheWritePricePerMillion)} cache write`,
  ].filter(Boolean);
  return (
    <div className="row model-row">
      <span className="row-main">
        <span className="model-title"><code title={model.id}>{model.id}</code>{model.isTee === true && <span className="badge">TEE</span>}</span>
        {model.name !== model.id && <span className="row-note" title={model.name}>{model.name}</span>}
        {capabilities.length > 0 && <span className="model-capabilities">{capabilities.join(" · ")}</span>}
      </span>
      <span className="model-meta">
        <span>{prices.length ? `${prices.join(" · ")} / 1M tokens` : "Pricing not reported"}</span>
        <span>{model.contextLength ? `${formatContext(model.contextLength)} context` : "Context not reported"}{model.maxOutputLength ? ` · ${formatContext(model.maxOutputLength)} max output` : ""}</span>
      </span>
    </div>
  );
}

function ConnectSheet({
  pending,
  models,
  catalogReady,
  applying,
  onModel,
  onCancel,
  onConfirm,
}: {
  pending: Pending;
  models: ModelSummary[];
  catalogReady: boolean;
  applying: boolean;
  onModel(model: string): void;
  onCancel(): void;
  onConfirm(): void;
}): React.JSX.Element {
  const { agent, connect, model, preview, error, loading } = pending;
  const dialog = useModalDialog(onCancel);
  const modelRequired = agent.id === "codex";
  return (
    <dialog ref={dialog} className="sheet" aria-label={`${connect ? "Connect" : "Disconnect"} ${agent.name}`}>
      <div className="sheet-heading">
        <AgentMark agent={agent} />
        <h2>{connect ? `Connect ${agent.name}` : `Disconnect ${agent.name}`}</h2>
      </div>
      {connect && agent.id !== "pi" && (
        <label className="sheet-field">
          <span>{modelRequired ? "Default model" : "Default model (optional)"}</span>
          <select
            aria-label={`Default model for ${agent.name}`}
            value={model}
            onChange={(event) => onModel(event.target.value)}
            disabled={applying || !catalogReady}
          >
            <option value="">{modelRequired ? "Select a verified model" : `Choose in ${agent.name}`}</option>
            {models.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.contextLength ? `${entry.id} · ${formatContext(entry.contextLength)}` : entry.id}
              </option>
            ))}
          </select>
        </label>
      )}
      {connect && agent.id === "pi" && (
        <p className="sheet-text">Pi discovers the verified model catalog automatically. Choose a model inside Pi after connecting.</p>
      )}
      <p className="sheet-text">
        {connect
          ? `After you connect, ${agent.name}'s AI requests go through ${brand.productName} to the verified service${model ? ` with ${model} as its default` : ""}. Available model choices come from the verified service, and previous settings return when you disconnect.`
          : `${agent.name} goes back to its previous settings; its local token is revoked.`}
      </p>
      {loading && <p className="sheet-text">Previewing changes…</p>}
      {error && <p className="sheet-text error" role="alert">{error}</p>}
      {preview && (
        <details className="disclosure">
          <summary>{preview.changes.length ? `Configuration changes (${preview.changes.length})` : "Configuration changes (none)"}</summary>
          <code className="config-path" title={agent.configPath}>{agent.configPath}</code>
          {preview.changes.length > 0 ? (
            <ul className="change-list">
              {preview.changes.map((change) => (
                <li key={change.key} className={change.sensitive ? "sensitive" : undefined}>
                  <code title={change.key}>{change.key}</code>
                  <span title={change.sensitive ? "Value hidden" : change.before ?? undefined}>{change.before ?? "(not set)"}</span>
                  <span aria-hidden="true">&rarr;</span>
                  <span title={change.sensitive ? "Value hidden" : change.after ?? undefined}>{change.after ?? "(removed)"}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="sheet-text">
              {connect
                ? "The config already points at the gateway; nothing will change."
                : "Nothing to restore; only the connection record will be cleared."}
            </p>
          )}
          <p className="sheet-text">{preview.note}</p>
        </details>
      )}
      {preview && agent.id === "opencode" && connect && (
        <p className="sheet-text">Restart OpenCode after connecting so it reloads its config.</p>
      )}
      <div className="sheet-actions">
        <button className="button" onClick={onCancel} disabled={applying}>
          Cancel
        </button>
        <button className={connect ? "button primary" : "button destructive"} onClick={onConfirm} disabled={applying || !preview}>
          {applying ? "Applying…" : connect ? "Connect" : "Disconnect"}
        </button>
      </div>
    </dialog>
  );
}

function RestoreAllSheet({
  applying,
  error,
  onCancel,
  onConfirm,
}: {
  applying: boolean;
  error?: string;
  onCancel(): void;
  onConfirm(): void;
}): React.JSX.Element {
  const dialog = useModalDialog(onCancel);
  return (
    <dialog ref={dialog} className="sheet" aria-label="Restore all agents">
      <div className="sheet-heading"><h2>Restore all agents?</h2></div>
      <p className="sheet-text">
        Every agent token is revoked first, then every recorded agent config is put back. This works
        even when the endpoint is unavailable or protection is off.
      </p>
      {error && <p className="sheet-text error" role="alert">{error}</p>}
      <div className="sheet-actions">
        <button className="button" onClick={onCancel} disabled={applying}>
          Cancel
        </button>
        <button className="button destructive" onClick={onConfirm} disabled={applying}>
          {applying ? "Restoring…" : "Restore All"}
        </button>
      </div>
    </dialog>
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
    <div className="row check-row" title={`${check.title}: ${check.detail}`}>
      <span className={`check-icon check-${check.status}`} aria-hidden="true">
        {check.status === "pass" && <Check size={12} />}
      </span>
      <span className="row-main">{title}</span>
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
      detail: "Port 4180 is in use, so agents cannot reach this app. Free it and relaunch.",
      tone: "danger",
    };
  }
  switch (state.status) {
    case "verifying":
      return { title: "Verifying…", detail: state.progress ?? "Checking the service before anything is sent.", tone: "neutral" };
    case "blocked":
      return { title: "Protection blocked", detail: state.error ?? "The verified identity or policy changed. Forwarding is fail-closed until a new verification succeeds.", tone: "danger" };
    case "error":
      return { title: "Verification failed", detail: "Nothing was sent. Check the service address and start again.", tone: "danger", settings: "Open Settings" };
    case "stopped":
      return { title: "Not protected", detail: "Start to verify the service and route your agents through it.", tone: "neutral" };
    case "verified":
      if (!state.apiKeySaved) {
        return { title: "API key needed", detail: `The service is verified. Add your ${brand.service.keyLabel} to start sending requests.`, tone: "warning", settings: "Add API key" };
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
  const prefix = key.startsWith("pag_") ? "pag_" : "";
  return `${prefix}${"•".repeat(12)}`;
}

function formatPrice(value: number): string {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: value < 0.1 ? 3 : 2,
    maximumFractionDigits: value < 0.1 ? 3 : 2,
  }).format(value);
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

function formatContext(tokens: number): string {
  return tokens >= 1_000_000
    ? `${(tokens / 1_000_000).toFixed(tokens % 1_000_000 === 0 ? 0 : 1)}M`
    : `${Math.round(tokens / 1_000)}K`;
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

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing renderer root");
}
createRoot(root).render(<App />);
