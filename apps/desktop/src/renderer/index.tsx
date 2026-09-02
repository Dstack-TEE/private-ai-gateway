import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Ban,
  Check,
  Clipboard,
  Laptop,
  LoaderCircle,
  Lock,
  LockOpen,
  RefreshCw,
  Server,
  ShieldCheck,
  ShieldX,
  TriangleAlert,
} from "lucide-react";

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
  VerificationCheck,
} from "../shared/contracts";
import "./styles.css";

// `?mock=<scenario>` renders the window against canned state for screenshots.
const query = new URLSearchParams(window.location.search);
const desktopApi: DesktopApi = query.has("mock") ? mockApi(query.get("mock")) : liveApi;

const INITIAL_STATE: GatewayState = {
  status: "stopped",
  checks: [],
  activity: [],
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

const AGENT_MARKS: Record<string, string> = { codex: "CX", "claude-code": "CC", opencode: "OC" };

type View = "overview" | "activity" | "settings";
type Tone = "success" | "warning" | "danger" | "neutral";
const VIEWS: { id: View; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "activity", label: "Activity" },
  { id: "settings", label: "Settings" },
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
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(INITIAL_STATE.config.remoteUrl);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [savingKey, setSavingKey] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [pending, setPending] = useState<Pending>();
  const [applying, setApplying] = useState(false);
  const [confirmRestoreAll, setConfirmRestoreAll] = useState(false);
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

  useEffect(() => {
    let active = true;
    const unsubscribe = desktopApi.onStateChange((nextState) => {
      if (active) {
        setState(nextState);
      }
    });
    const unsubscribeNavigate = desktopApi.onNavigate((section) => active && setView(section));
    void desktopApi.getState().then(
      (nextState) => active && setState(nextState),
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    return () => {
      active = false;
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

  const saveApiKey = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!apiKeyDraft.trim()) {
      return;
    }
    setSavingKey(true);
    await run(async () => {
      const next = await desktopApi.setApiKey(apiKeyDraft);
      setApiKeyDraft("");
      return next;
    });
    setSavingKey(false);
  };

  const refreshCatalog = async () => {
    setRefreshing(true);
    await run(() => desktopApi.refreshCatalog());
    setRefreshing(false);
  };

  const copy = async (label: string, value: string) => {
    await run(async () => {
      await desktopApi.copyText(value);
      setCopied(label);
      window.setTimeout(
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
      const preview = await desktopApi.previewAgent(agent.id, connect, connect ? { model } : {});
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
    if (connect) {
      setPending({ agent, connect, model: "", loading: false });
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
    await run(async () => {
      await desktopApi.applyAgent(agent.id, connect, preview.revision, connect ? { model } : {});
      setPending(undefined);
      await loadAgents();
    });
    setApplying(false);
  };

  const restoreAll = async () => {
    setApplying(true);
    await run(async () => {
      setAgents(await desktopApi.disconnectAllAgents());
      setConfirmRestoreAll(false);
    });
    setApplying(false);
  };

  const anyRecorded = agents.some((agent) => agent.recorded);
  const problem = actionError ?? state.error;
  const locked = Boolean(pending) || applying;

  return (
    <main className="window">
      <div className="toolbar">
        <SegmentedControl value={view} onChange={setView} />
      </div>

      <div className="content" role="tabpanel" id={`panel-${view}`} aria-labelledby={`tab-${view}`} key={view}>
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
            onToggle={toggleGateway}
            onSettings={() => setView("settings")}
            onSelect={openPending}
          />
        )}
        {view === "activity" && <ActivityView state={state} running={running} problem={problem} />}
        {view === "settings" && (
          <SettingsView
            state={state}
            busy={busy}
            running={running}
            verified={verified}
            remoteUrl={remoteUrl}
            requireProductionOs={requireProductionOs}
            apiKeyDraft={apiKeyDraft}
            savingKey={savingKey}
            refreshing={refreshing}
            copied={copied}
            anyRecorded={anyRecorded}
            locked={locked}
            problem={problem}
            onRemoteUrl={setRemoteUrl}
            onPolicy={setRequireProductionOs}
            onApiKeyDraft={setApiKeyDraft}
            onSaveKey={saveApiKey}
            onClearKey={() => void run(() => desktopApi.clearApiKey())}
            onRefresh={refreshCatalog}
            onCopy={copy}
            onRestoreAll={() => setConfirmRestoreAll(true)}
            onSupport={() => void run(() => desktopApi.openSupport())}
          />
        )}
      </div>

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
          onCancel={() => setConfirmRestoreAll(false)}
          onConfirm={() => void restoreAll()}
        />
      )}
    </main>
  );
}

/** Toolbar view switcher: a tab list with arrow-key movement. */
function SegmentedControl({ value, onChange }: { value: View; onChange(view: View): void }): React.JSX.Element {
  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const index = VIEWS.findIndex((entry) => entry.id === value);
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (step === 0) {
      return;
    }
    event.preventDefault();
    const next = VIEWS[(index + step + VIEWS.length) % VIEWS.length]?.id ?? value;
    onChange(next);
    (event.currentTarget.querySelector(`#tab-${next}`) as HTMLElement | null)?.focus();
  };
  return (
    <div className="segmented" role="tablist" aria-label="View" onKeyDown={onKeyDown}>
      {VIEWS.map((entry) => (
        <button
          key={entry.id}
          id={`tab-${entry.id}`}
          role="tab"
          aria-selected={value === entry.id}
          aria-controls={`panel-${entry.id}`}
          tabIndex={value === entry.id ? 0 : -1}
          onClick={() => onChange(entry.id)}
        >
          {entry.label}
        </button>
      ))}
    </div>
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
  onToggle,
  onSettings,
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
  onToggle(): void;
  onSettings(): void;
  onSelect(agent: AgentStatus, connect: boolean): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const connected = agents.filter((agent) => agent.connected).length;
  return (
    <>
      <section className={`hero tone-${verdict.tone}`} aria-label="Protection status">
        <Tunnel tone={verdict.tone} busy={busy} />
        <h1 aria-live="polite">{verdict.title}</h1>
        <p>{verdict.detail}</p>
        {problem && (
          <p className="hero-problem" role="alert">
            <TriangleAlert size={14} aria-hidden="true" /> {problem}
          </p>
        )}
        <div className="hero-actions">
          {running || busy ? (
            <button className="button large" onClick={onToggle}>
              {busy ? "Cancel" : "Stop"}
            </button>
          ) : (
            <button
              className="button primary large"
              onClick={onToggle}
              disabled={endpointDown}
              title={endpointDown ? state.endpointError : undefined}
            >
              Start
            </button>
          )}
          {verdict.settings && (
            <button className="link" onClick={onSettings}>
              {verdict.settings}
            </button>
          )}
        </div>
      </section>

      <section className="group" aria-labelledby="agents-title">
        <h2 className="group-title" id="agents-title">
          Agents
          <span>{agents.length ? `${connected} of ${agents.length} connected` : ""}</span>
        </h2>
        <div className="inset">
          {agents.length === 0 && <EmptyState text="Agent configs unavailable" />}
          {agents.map((agent) => (
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
    </>
  );
}

/** Device to verified private AI: the path a request takes, and its state. */
function Tunnel({ tone, busy }: { tone: Tone; busy: boolean }): React.JSX.Element {
  const Link = tone === "success" ? Lock : busy ? LoaderCircle : LockOpen;
  return (
    <div className={`tunnel tunnel-${tone}`} aria-hidden="true">
      <div className="tunnel-node">
        <Laptop size={22} />
        <span>This device</span>
      </div>
      <div className="tunnel-link">
        <span className="tunnel-line" />
        <span className="tunnel-lock">
          <Link size={14} className={busy ? "spin" : undefined} />
        </span>
        <span className="tunnel-line" />
      </div>
      <div className="tunnel-node">
        <Server size={22} />
        <span>Verified private AI</span>
      </div>
    </div>
  );
}

function AgentRow({
  agent,
  disabled,
  connectBlocked,
  onSelect,
}: {
  agent: AgentStatus;
  disabled: boolean;
  /** Connecting needs the verified catalog and a bound endpoint; disconnecting never does. */
  connectBlocked: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
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
    <div className="agent-block row" title={agent.configPath}>
      <span className={`mark ${agent.connected ? "mark-on" : ""}`} aria-hidden="true">
        {AGENT_MARKS[agent.id] ?? agent.name.slice(0, 2).toUpperCase()}
      </span>
      <div className="row-main">
        <span className="row-title">{agent.name}</span>
        <StateLabel tone={presence.tone} icon={presence.icon} text={presence.label} />
        {note && <p className="row-note">{note}</p>}
      </div>
      <button
        className="button"
        disabled={disabled || !actionable}
        title={!disconnecting && connectBlocked ? "Start protection first; models come from the verified service" : undefined}
        onClick={() => onSelect(!disconnecting)}
      >
        {disconnecting ? "Disconnect" : "Connect"}
      </button>
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

function ActivityView({
  state,
  running,
  problem,
}: {
  state: GatewayState;
  running: boolean;
  problem?: string;
}): React.JSX.Element {
  const [selected, setSelected] = useState<string>();
  const keyOf = (item: RequestActivity, index: number) => item.receiptId ?? `${item.at}-${item.path}-${index}`;
  const current = state.activity.map((item, index) => [keyOf(item, index), item] as const).find(([key]) => key === selected)?.[1];
  const outcomes = summarize(state.activity);
  return (
    <div className="split">
      {problem && <p className="banner" role="alert">{problem}</p>}
      <section className="group" aria-labelledby="activity-title">
        <h2 className="group-title" id="activity-title">
          Recent requests
          <span>
            {state.activity.length
              ? `${outcomes.protected} protected${outcomes.blocked ? `, ${outcomes.blocked} blocked` : ""}${outcomes.failed ? `, ${outcomes.failed} failed proof` : ""}`
              : ""}
          </span>
        </h2>
        <div className="inset list">
          {state.activity.length === 0 && (
            <EmptyState
              text={running ? "No requests yet. Send one from a connected agent." : "Start protection to see requests and their proofs."}
            />
          )}
          {state.activity.length > 0 && (
            <ul className="list-items" aria-label="Recent requests">
              {state.activity.map((item, index) => {
                const key = keyOf(item, index);
                const outcome = outcomeOf(item);
                return (
                  <li key={key}>
                    <button
                      className="row list-row"
                      aria-pressed={selected === key}
                      onClick={() => setSelected((value) => (value === key ? undefined : key))}
                    >
                      <div className="row-main">
                        <span className="row-title">{agentName(item.agent)}</span>
                        <StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} />
                        <p className="row-note">
                          <code>{item.path}</code>
                        </p>
                      </div>
                      <time className="row-side">{formatTimestamp(item.at * 1_000)}</time>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </section>
      <aside className="inspector" aria-live="polite" aria-label="Request details">
        {current ? <Evidence activity={current} /> : <p className="inspector-hint">Select a request to see its proof.</p>}
      </aside>
    </div>
  );
}

function Evidence({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const failed = activity.status < 200 || activity.status >= 300;
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
      <dt>Outcome</dt>
      <dd>
        <StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} />
        {failed && <span className="dim"> HTTP {activity.status}</span>}
        {activity.detail && <span className="dim"> · {activity.detail}</span>}
      </dd>
      {activity.receiptId && (
        <>
          <dt>Proof</dt>
          <dd>
            {activity.verified === true
              ? "Signed receipt verified: request and response bytes match what this app sent and received."
              : activity.verified === false
                ? "Signed receipt did not verify."
                : "Receipt not checked yet."}
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
  verified,
  remoteUrl,
  requireProductionOs,
  apiKeyDraft,
  savingKey,
  refreshing,
  copied,
  anyRecorded,
  locked,
  problem,
  onRemoteUrl,
  onPolicy,
  onApiKeyDraft,
  onSaveKey,
  onClearKey,
  onRefresh,
  onCopy,
  onRestoreAll,
  onSupport,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  verified: boolean;
  remoteUrl: string;
  requireProductionOs: boolean;
  apiKeyDraft: string;
  savingKey: boolean;
  refreshing: boolean;
  copied?: string;
  anyRecorded: boolean;
  locked: boolean;
  problem?: string;
  onRemoteUrl(value: string): void;
  onPolicy(value: boolean): void;
  onApiKeyDraft(value: string): void;
  onSaveKey(event: React.FormEvent): Promise<void>;
  onClearKey(): void;
  onRefresh(): Promise<void>;
  onCopy(label: string, value: string): Promise<void>;
  onRestoreAll(): void;
  onSupport(): void;
}): React.JSX.Element {
  const models = state.catalog?.models ?? [];
  const frozen = busy || running;
  return (
    <>
      {problem && <p className="banner" role="alert">{problem}</p>}

      <section className="group" aria-labelledby="general-title">
        <h2 className="group-title" id="general-title">
          General
          {frozen && <span>Stop protection to change the service</span>}
        </h2>
        <div className="inset">
          <label className="row field-row">
            <span className="row-main">AI service</span>
            <input value={remoteUrl} onChange={(event) => onRemoteUrl(event.target.value)} disabled={frozen} spellCheck={false} />
          </label>
          <label className="row toggle-row">
            <span className="row-main">Require production OS</span>
            <input type="checkbox" checked={requireProductionOs} onChange={(event) => onPolicy(event.target.checked)} disabled={frozen} />
            <span className="toggle-track" aria-hidden="true"><span /></span>
          </label>
          <div className="row">
            <span className="row-main">
              {brand.service.keyLabel}
              <span className="row-note">
                {state.apiKeySaved ? "Saved in the system credential store; used only by this app." : "Not saved. Agents get their own local tokens; the key never reaches them."}
              </span>
            </span>
            {state.apiKeySaved && (
              <button type="button" className="button" onClick={onClearKey} disabled={savingKey}>
                Delete
              </button>
            )}
          </div>
          <form className="row field-row" onSubmit={(event) => void onSaveKey(event)}>
            <input
              type="password"
              value={apiKeyDraft}
              onChange={(event) => onApiKeyDraft(event.target.value)}
              placeholder={state.apiKeySaved ? "Replace with a new key" : `Paste your ${brand.service.keyLabel}`}
              autoComplete="off"
              spellCheck={false}
              aria-label={brand.service.keyLabel}
            />
            <button type="submit" className="button" disabled={savingKey || !apiKeyDraft.trim()}>
              {savingKey ? "Saving…" : state.apiKeySaved ? "Replace" : "Save"}
            </button>
          </form>
        </div>
      </section>

      <PrivacyVerification state={state} verified={verified} />

      {anyRecorded && (
        <section className="group" aria-labelledby="agents-settings-title">
          <h2 className="group-title" id="agents-settings-title">Agents</h2>
          <div className="inset">
            <div className="row">
              <span className="row-main">
                Restore all agent configs
                <span className="row-note">Revokes every agent token and puts every config back, even while protection is off.</span>
              </span>
              <button className="button" disabled={locked} onClick={onRestoreAll}>
                Restore All…
              </button>
            </div>
          </div>
        </section>
      )}

      <section className="group" aria-labelledby="advanced-title">
        <h2 className="group-title" id="advanced-title">
          Advanced
          <span>{state.proxyUrl ? "Endpoint bound" : "Endpoint unavailable"}</span>
        </h2>
        <div className="inset">
          {state.endpointError && <p className="row-warning">{state.endpointError}</p>}
          <EndpointRow label="OpenAI-style endpoint" value={openAiEndpoint(state.proxyUrl)} copied={copied} onCopy={onCopy} />
          <EndpointRow label="Anthropic-style endpoint" value={state.proxyUrl} copied={copied} onCopy={onCopy} />
          <p className="row-footnote">
            On this device only: agents reach the app over a plain local connection that never
            leaves the machine. Requests are relayed to the verified service as sent; the service
            decides what it answers.
          </p>
          <div className="row">
            <span className="row-main">
              Models
              <span className="row-note">{state.catalog ? `${models.length} from the verified service` : "Not loaded until the service is verified"}</span>
            </span>
            <button
              className="button icon"
              onClick={() => void onRefresh()}
              disabled={!verified || refreshing}
              aria-label="Refresh the model list from the verified service"
              title="Refresh from the verified service"
            >
              <RefreshCw size={15} className={refreshing ? "spin" : undefined} />
            </button>
          </div>
          {state.catalog && state.catalog.removed.length > 0 && (
            <p className="row-warning">
              No longer served: {state.catalog.removed.join(", ")}. Agents set to these models need a
              new choice; nothing is switched for you.
            </p>
          )}
          {models.map((model) => <ModelRow key={model.id} model={model} />)}
        </div>
      </section>

      <section className="group" aria-label="About">
        <h2 className="group-title">About</h2>
        <div className="inset about">
          <picture>
            <source srcSet={brand.wordmark.dark} media="(prefers-color-scheme: dark)" />
            <img src={brand.wordmark.light} alt={brand.organizationName} className="wordmark" />
          </picture>
          <p>
            <strong>{brand.productName}</strong> by {brand.organizationName}. {brand.tagline}.
          </p>
          <button className="link" onClick={onSupport}>{brand.organizationName} Support</button>
        </div>
      </section>
    </>
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
      title: "Encrypted outside this device",
      detail: verified
        ? "The connection leaving this device is encrypted to the service's own attested key, so the operator and anyone on the network see only ciphertext."
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
      <h2 className="group-title">
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
              <Detail label="E2EE" value={identity.supportedE2eeVersions.join(", ") || "None"} mono />
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
    <div className="row">
      <span className="row-main">
        {label}
        <code className="row-note" title={value}>{value ?? "–"}</code>
      </span>
      <button
        className="button icon"
        disabled={!value}
        onClick={() => value && void onCopy(label, value)}
        aria-label={`Copy ${label}`}
        title={`Copy ${label}`}
      >
        {copied === label ? <Check size={15} className="copied" /> : <Clipboard size={15} />}
      </button>
    </div>
  );
}

function ModelRow({ model }: { model: ModelSummary }): React.JSX.Element {
  return (
    <div className="row model-row">
      <span className="row-main">
        <code title={model.id}>{model.id}</code>
        {model.name !== model.id && <span className="row-note" title={model.name}>{model.name}</span>}
      </span>
      {model.contextLength && <span className="row-side">{formatContext(model.contextLength)} ctx</span>}
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
  return (
    <dialog ref={dialog} className="sheet" aria-label={`${connect ? "Connect" : "Disconnect"} ${agent.name}`}>
      <div className="sheet-heading">
        <span className="mark" aria-hidden="true">{AGENT_MARKS[agent.id] ?? agent.name.slice(0, 2).toUpperCase()}</span>
        <h2>{connect ? `Connect ${agent.name}` : `Disconnect ${agent.name}`}</h2>
      </div>
      {connect && (
        <label className="sheet-field">
          <span>Model</span>
          <select
            aria-label={`Model for ${agent.name}`}
            value={model}
            onChange={(event) => onModel(event.target.value)}
            disabled={applying || !catalogReady}
          >
            <option value="">Choose a verified model</option>
            {models.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.contextLength ? `${entry.id} · ${formatContext(entry.contextLength)}` : entry.id}
              </option>
            ))}
          </select>
        </label>
      )}
      <p className="sheet-text">
        {connect
          ? `After you connect, ${agent.name}'s AI requests go through ${brand.productName} to the verified service${model ? ` using ${model}` : ""}. Your previous settings are kept and come back when you disconnect.`
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
  onCancel,
  onConfirm,
}: {
  applying: boolean;
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
      return { title: "Verifying…", detail: state.progress ?? "Checking the service before anything is sent.", tone: "warning" };
    case "blocked":
      return { title: "Blocked", detail: "The service changed identity mid-session. Nothing is sent until it verifies again.", tone: "danger" };
    case "error":
      return { title: "Verification failed", detail: "Nothing was sent. Check the service address and start again.", tone: "danger", settings: "Open Settings" };
    case "stopped":
      return { title: "Not protected", detail: "Start to verify the service and route your agents through it.", tone: "neutral" };
    case "verified":
      if (!state.apiKeySaved) {
        return { title: "API key needed", detail: `The service is verified. Add your ${brand.service.keyLabel} to start sending requests.`, tone: "warning", settings: "Add API key" };
      }
      return { title: "Protected", detail: "Your prompts stay private: encrypted to a verified confidential AI service, with a signed proof for every answer.", tone: "success" };
  }
}

/** The plain-language outcome of one request. */
function outcomeOf(activity: RequestActivity): { label: string; tone: Tone; icon: typeof ShieldCheck } {
  if (activity.verified === true) {
    return { label: "Protected", tone: "success", icon: ShieldCheck };
  }
  if (activity.verified === false) {
    return { label: "Verification failed", tone: "danger", icon: TriangleAlert };
  }
  if (activity.receiptId) {
    return { label: "Checking proof", tone: "warning", icon: LoaderCircle };
  }
  return activity.status >= 400
    ? { label: "Blocked", tone: "danger", icon: Ban }
    : { label: "No proof", tone: "neutral", icon: ShieldX };
}

function summarize(activity: RequestActivity[]): { protected: number; blocked: number; failed: number } {
  return activity.reduce(
    (totals, item) => {
      if (item.verified === true) totals.protected += 1;
      else if (item.verified === false) totals.failed += 1;
      else if (!item.receiptId && item.status >= 400) totals.blocked += 1;
      return totals;
    },
    { protected: 0, blocked: 0, failed: 0 },
  );
}

function agentName(id?: string): string {
  switch (id) {
    case "codex": return "Codex";
    case "claude-code": return "Claude Code";
    case "opencode": return "OpenCode";
    default: return id ?? "Unknown client";
  }
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
