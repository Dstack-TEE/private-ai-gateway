import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Check,
  CheckCircle2,
  CircleStop,
  Clipboard,
  KeyRound,
  LoaderCircle,
  Play,
  RefreshCw,
  ShieldCheck,
  ShieldX,
} from "lucide-react";

import { desktopApi as liveApi } from "./desktop-api";
import { mockApi } from "./mock-api";
import type {
  AgentPreview,
  AgentStatus,
  ConnectOptions,
  DesktopApi,
  GatewayState,
  ModelSummary,
  RequestActivity,
  Surface,
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
  config: { remoteUrl: "", requireProductionOs: false },
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

const SURFACE_LABEL: Record<Surface, string> = {
  responses: "Responses API",
  messages: "Messages API",
  chat_completions: "Chat Completions",
};

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
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(INITIAL_STATE.config.remoteUrl);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [savingKey, setSavingKey] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const [agents, setAgents] = useState<AgentStatus[]>([]);
  const [claudeModel, setClaudeModel] = useState("");
  const [preview, setPreview] = useState<AgentPreview>();
  const [previewOptions, setPreviewOptions] = useState<ConnectOptions>({});
  const [applying, setApplying] = useState(false);
  const [confirmRestoreAll, setConfirmRestoreAll] = useState(false);
  const busy = state.status === "verifying";
  const running = state.status === "verified" || state.status === "blocked";
  const verified = state.status === "verified";
  const endpointDown = Boolean(state.endpointError);
  const verdict = presentation(state);
  const VerdictIcon = verdict.icon;

  useEffect(() => {
    let active = true;
    const unsubscribe = desktopApi.onStateChange((nextState) => {
      if (active) {
        setState(nextState);
      }
    });
    void desktopApi.getState().then(
      (nextState) => active && setState(nextState),
      (error: unknown) => active && setActionError(errorMessage(error)),
    );
    return () => {
      active = false;
      unsubscribe();
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

  const openPreview = (agent: AgentStatus, connect: boolean) => {
    const options: ConnectOptions = agent.id === "claude-code" && connect ? { model: claudeModel } : {};
    return run(async () => {
      setPreviewOptions(options);
      setPreview(await desktopApi.previewAgent(agent.id, connect, options));
    });
  };

  const confirmPreview = async () => {
    if (!preview) {
      return;
    }
    setApplying(true);
    await run(async () => {
      await desktopApi.applyAgent(preview.agent.id, preview.connect, preview.revision, previewOptions);
      setPreview(undefined);
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

  const models = state.catalog?.models ?? [];
  const connectedAgents = agents.filter((agent) => agent.connected).length;
  const anyRecorded = agents.some((agent) => agent.recorded);

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-mark" aria-hidden="true"><ShieldCheck size={18} /></div>
        <div className="brand-copy">
          <h1>Private AI Gateway</h1>
          <span>Verified local endpoint for coding agents</span>
        </div>
        <div className={`status-badge status-${verdict.tone}`}>
          <VerdictIcon size={14} className={busy ? "spin" : undefined} />
          {verdict.label}
        </div>
      </header>

      <section className="verdict-bar" aria-label="Gateway status">
        <p className={`verdict verdict-${verdict.tone}`} aria-live="polite">
          <VerdictIcon size={15} className={busy ? "spin" : undefined} />
          <span>{verdict.summary}</span>
        </p>
        {running || busy ? (
          <button className="command-button stop-button" onClick={() => void run(() => desktopApi.stop())}>
            <CircleStop size={15} /> {busy ? "Cancel" : "Stop"}
          </button>
        ) : (
          <button
            className="command-button start-button"
            onClick={() => void run(() => desktopApi.start({ remoteUrl, requireProductionOs }))}
            disabled={endpointDown}
            title={endpointDown ? state.endpointError : undefined}
          >
            <Play size={15} /> Start
          </button>
        )}
      </section>

      {state.endpointError && (
        <div className="error-banner" role="alert">
          <ShieldX size={16} />
          <span>
            {state.endpointError}. Nothing can start or connect until the app is relaunched with the
            endpoint free; agents can still be disconnected below.
          </span>
        </div>
      )}
      {(actionError || state.error) && (
        <div className="error-banner" role="alert">
          <ShieldX size={16} />
          <span>{actionError ?? state.error}</span>
        </div>
      )}

      <section className="setup-section" aria-label="Service setup">
        <SectionHeading title="Service" meta={remoteHost(state.remoteUrl ?? remoteUrl)} />
        <div className="setup-body">
          <label className="url-field">
            <span>AI service</span>
            <input
              value={remoteUrl}
              onChange={(event) => setRemoteUrl(event.target.value)}
              disabled={busy || running}
              spellCheck={false}
            />
          </label>
          <label className="policy-toggle">
            <input
              type="checkbox"
              checked={requireProductionOs}
              onChange={(event) => setRequireProductionOs(event.target.checked)}
              disabled={busy || running}
            />
            <span className="toggle-track" aria-hidden="true"><span /></span>
            Require production OS
          </label>
        </div>
        <div className="key-row">
          <div className={`key-status ${state.apiKeySaved ? "key-saved" : ""}`}>
            <KeyRound size={14} />
            <div>
              <strong>{state.apiKeySaved ? "API key saved in the system credential store" : "No API key saved"}</strong>
              <span>Used only inside this app. Agents get their own local tokens, never this key.</span>
            </div>
          </div>
          <form className="key-form" onSubmit={(event) => void saveApiKey(event)}>
            <input
              type="password"
              value={apiKeyDraft}
              onChange={(event) => setApiKeyDraft(event.target.value)}
              placeholder={state.apiKeySaved ? "Replace with a new key" : "RedPill API key"}
              autoComplete="off"
              spellCheck={false}
              aria-label="RedPill API key"
            />
            <button type="submit" className="small-button" disabled={savingKey || !apiKeyDraft.trim()}>
              {savingKey ? "Saving..." : state.apiKeySaved ? "Replace" : "Save"}
            </button>
            {state.apiKeySaved && (
              <button
                type="button"
                className="small-button danger-button"
                onClick={() => void run(() => desktopApi.clearApiKey())}
                disabled={savingKey}
              >
                Delete
              </button>
            )}
          </form>
        </div>
      </section>

      <section className="endpoint-section" aria-label="Local endpoint">
        <SectionHeading title="Point your agent here" meta={state.proxyUrl ? "Bound" : "Unavailable"} />
        <EndpointRow label="OpenAI-style" value={openAiEndpoint(state.proxyUrl)} copied={copied} onCopy={copy} />
        <EndpointRow label="Anthropic-style" value={state.proxyUrl} copied={copied} onCopy={copy} />
        <p className="section-note">
          Served over TLS with this installation's own certificate, so an agent can tell this app apart
          from anything else on the port; connected agents trust it through NODE_EXTRA_CA_CERTS.
        </p>
      </section>

      <section className="agents-section" aria-label="Connected agents">
        <div className="section-heading">
          <h2>Connected agents</h2>
          <span>{`${connectedAgents}/${agents.length || 3}`}</span>
          {anyRecorded && (
            <button
              className="small-button danger-button"
              disabled={applying || Boolean(preview)}
              onClick={() => setConfirmRestoreAll(true)}
              title="Revoke every agent token and restore every agent config, even when an agent is unsupported or the endpoint is down"
            >
              Restore all
            </button>
          )}
        </div>
        <p className="section-note">
          Each agent gets a revocable local token that only opens its own endpoints and labels its
          requests; tokens do not protect against other software running as you. Model choices are
          yours; OAuth logins are not converted.
        </p>
        {confirmRestoreAll && (
          <RestoreAllDialog
            applying={applying}
            onCancel={() => setConfirmRestoreAll(false)}
            onConfirm={() => void restoreAll()}
          />
        )}
        {agents.length === 0 && <EmptyState text="Agent configs unavailable" />}
        {agents.map((agent) => (
          <React.Fragment key={agent.id}>
            <AgentRow
              agent={agent}
              models={models}
              claudeModel={claudeModel}
              onClaudeModel={setClaudeModel}
              disabled={Boolean(preview) || applying}
              connectBlocked={endpointDown}
              catalogReady={verified && models.length > 0}
              onSelect={(connect) => void openPreview(agent, connect)}
            />
            {preview?.agent.id === agent.id && (
              <PreviewPanel
                preview={preview}
                applying={applying}
                onCancel={() => setPreview(undefined)}
                onConfirm={() => void confirmPreview()}
              />
            )}
          </React.Fragment>
        ))}
      </section>

      <section className="models-section" aria-label="Models">
        <div className="section-heading">
          <h2>Models</h2>
          <span>{state.catalog ? `${models.length} from verified service` : "Not loaded"}</span>
          <button
            className="icon-button"
            onClick={() => void refreshCatalog()}
            disabled={!verified || refreshing}
            aria-label="Refresh the model list from the verified service"
            title="Refresh from the verified service"
          >
            <RefreshCw size={14} className={refreshing ? "spin" : undefined} />
          </button>
        </div>
        {state.catalog && state.catalog.removed.length > 0 && (
          <p className="section-warning">
            No longer served: {state.catalog.removed.join(", ")}. Agents set to these models need a
            new choice; nothing is switched for you.
          </p>
        )}
        {models.length > 0 ? (
          <>
            <p className="section-note">
              Availability comes from the verified service's model list. A surface is used only when the
              service declares it for every listed model (aci_capabilities v1); undeclared surfaces are refused.
            </p>
            <div className="model-list">
              {models.map((model) => <ModelRow key={model.id} model={model} />)}
            </div>
          </>
        ) : (
          <EmptyState text={busy ? (state.progress ?? "Verifying...") : "The model list comes from the verified service; it is empty while the gateway is not verified"} />
        )}
      </section>

      <section className="requests-section" aria-label="Recent requests">
        <SectionHeading title="Recent requests" meta={String(state.activity.length)} />
        <RequestAudit activity={state.activity} running={running} />
      </section>

      <details className="technical-details">
        <summary>
          Technical details
          {state.checks.length > 0 && <span>{checkCount(state.checks)} checks passed</span>}
        </summary>
        {state.identity ? (
          <IdentityDetails state={state} />
        ) : (
          <EmptyState text="Verification details appear once the service is verified" />
        )}
      </details>
    </main>
  );
}

function SectionHeading({ title, meta }: { title: string; meta?: string }): React.JSX.Element {
  return (
    <div className="section-heading">
      <h2>{title}</h2>
      {meta && <span>{meta}</span>}
    </div>
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
    <div className="endpoint-row">
      <span>{label}</span>
      <code title={value}>{value ?? "-"}</code>
      <button
        className="icon-button"
        disabled={!value}
        onClick={() => value && void onCopy(label, value)}
        aria-label={`Copy ${label} endpoint`}
        title={`Copy ${label} endpoint`}
      >
        {copied === label ? <CheckCircle2 size={15} /> : <Clipboard size={15} />}
      </button>
    </div>
  );
}

function AgentRow({
  agent,
  models,
  claudeModel,
  onClaudeModel,
  disabled,
  connectBlocked,
  catalogReady,
  onSelect,
}: {
  agent: AgentStatus;
  models: ModelSummary[];
  claudeModel: string;
  onClaudeModel(model: string): void;
  disabled: boolean;
  /** Connecting is blocked (endpoint down); disconnecting never is. */
  connectBlocked: boolean;
  catalogReady: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const presence = agent.attention && (agent.connected || !agent.supported)
    ? { label: "Needs attention", tone: "unverified" }
    : !agent.supported
    ? { label: "Unsupported", tone: "neutral" }
    : agent.error
      ? { label: "Config error", tone: "failed" }
      : agent.attention
        ? { label: "Needs attention", tone: "unverified" }
        : agent.connected
          ? { label: "Connected", tone: "verified" }
          : agent.installed
            ? { label: "Not connected", tone: "neutral" }
            : { label: "CLI not found", tone: "neutral" };
  const needsModel = agent.id === "claude-code" && !agent.recorded;
  const needsCatalog = agent.id !== "codex" && !agent.recorded && !catalogReady;
  const disconnecting = agent.recorded;
  // Disconnect must stay available whatever the config's state.
  // A missing CLI is only a hint: connecting creates the official config
  // from scratch. Disconnect stays available whatever the config's state.
  const actionable = disconnecting
    ? true
    : !connectBlocked &&
      agent.supported &&
      !agent.error &&
      !(needsModel && !claudeModel) &&
      !needsCatalog;
  const note =
    agent.reason ??
    agent.attention ??
    agent.error ??
    (!agent.installed && agent.supported
      ? `The ${agent.name} CLI was not detected; connecting still writes its official config.`
      : undefined);
  return (
    <div className="agent-block">
      <div className="agent-row" title={agent.configPath}>
        <div className="agent-name">
          <span>{agent.name}</span>
          <small>{SURFACE_LABEL[agent.surface]}</small>
        </div>
        <span className={`chip chip-${presence.tone}`}>{presence.label}</span>
        <button
          className="small-button"
          disabled={disabled || !actionable}
          title={needsCatalog && !disconnecting ? "Start the gateway; the model list comes from the verified service" : undefined}
          onClick={() => onSelect(!disconnecting)}
        >
          {disconnecting ? "Disconnect" : "Connect"}
        </button>
      </div>
      {needsModel && agent.supported && !agent.error && !disconnecting && (
        <label className="agent-select">
          <span>Model for Claude Code</span>
          <select
            value={claudeModel}
            onChange={(event) => onClaudeModel(event.target.value)}
            disabled={disabled || !catalogReady}
          >
            <option value="">{catalogReady ? "Choose a verified model" : "Start the gateway to load models"}</option>
            {models.map((model) => (
              <option key={model.id} value={model.id}>{model.id}</option>
            ))}
          </select>
        </label>
      )}
      {note && <p className="agent-note">{note}</p>}
    </div>
  );
}

function RestoreAllDialog({
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
    <dialog ref={dialog} className="preview-panel" aria-label="Restore all agents">
      <div className="preview-heading"><strong>Restore all agents</strong></div>
      <p className="preview-note">
        Revokes every agent token first, then restores every recorded agent config. Works even when
        an agent is no longer supported, the endpoint is unavailable, or the gateway is stopped.
      </p>
      <div className="preview-actions">
        <button className="command-button ghost-button" onClick={onCancel} disabled={applying}>
          Cancel
        </button>
        <button className="command-button stop-button" onClick={onConfirm} disabled={applying}>
          {applying ? "Restoring..." : "Restore all"}
        </button>
      </div>
    </dialog>
  );
}

function PreviewPanel({
  preview,
  applying,
  onCancel,
  onConfirm,
}: {
  preview: AgentPreview;
  applying: boolean;
  onCancel(): void;
  onConfirm(): void;
}): React.JSX.Element {
  const verb = preview.connect ? "Connect" : "Disconnect";
  const dialog = useModalDialog(onCancel);
  return (
    <dialog ref={dialog} className="preview-panel" aria-label={`${verb} ${preview.agent.name}`}>
      <div className="preview-heading">
        <strong>{verb} {preview.agent.name}</strong>
        <code title={preview.agent.configPath}>{preview.agent.configPath}</code>
      </div>
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
        <p className="preview-note">
          {preview.connect
            ? "The config already points at the gateway; nothing will change."
            : "Nothing to restore; only the connection record will be cleared."}
        </p>
      )}
      <p className="preview-note">{preview.note}</p>
      {preview.agent.id === "opencode" && preview.connect && (
        <p className="preview-note">Restart OpenCode after applying so it reloads its config.</p>
      )}
      <div className="preview-actions">
        <button className="command-button ghost-button" onClick={onCancel} disabled={applying}>
          Cancel
        </button>
        <button className="command-button start-button" onClick={onConfirm} disabled={applying}>
          {applying ? "Applying..." : preview.connect ? "Apply changes" : "Restore"}
        </button>
      </div>
    </dialog>
  );
}

function ModelRow({ model }: { model: ModelSummary }): React.JSX.Element {
  const declared: string[] = [];
  if (model.chatCompletions.level === "declared") declared.push("Chat Completions");
  if (model.messages.level === "declared") declared.push("Messages");
  if (model.responses.level === "declared") declared.push("Responses");
  return (
    <div className="model-row">
      <div className="model-name">
        <code title={model.id}>{model.id}</code>
        <span>
          {model.name !== model.id && <span>{model.name}</span>}
          {model.contextLength && <span>{formatContext(model.contextLength)} context</span>}
        </span>
      </div>
      <div className="model-support">
        {declared.length > 0 ? (
          <span className="chip chip-verified" title="Declared by the service's aci_capabilities v1 for every listed model">
            Declared: {declared.join(", ")}
          </span>
        ) : (
          <span className="chip chip-unverified" title="The service publishes no capability declaration; requests on every surface are refused">
            Undeclared by service · not routed
          </span>
        )}
      </div>
    </div>
  );
}

function RequestAudit({
  activity,
  running,
}: {
  activity: RequestActivity[];
  running: boolean;
}): React.JSX.Element {
  if (activity.length === 0) {
    return (
      <EmptyState
        text={running ? "No requests yet - send one from a connected agent" : "Start the gateway to see requests"}
      />
    );
  }
  return (
    <div className="request-list">
      {activity.map((item, index) => (
        <ActivityRow key={item.receiptId ?? `${item.at}-${item.path}-${index}`} activity={item} />
      ))}
    </div>
  );
}

function ActivityRow({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const audit = auditPresentation(activity);
  const route = routePresentation(activity);
  return (
    <article className="audit-row" title={activity.detail}>
      <div className="audit-mainline">
        <span className="agent-label">{agentName(activity.agent)}</span>
        <code title={activity.path}>{activity.path}</code>
        <span className={`chip chip-${audit.tone}`}>{audit.label}</span>
      </div>
      <div className="audit-meta">
        {route && <span className={`route route-${route.tone}`} title={route.title}>{route.label}</span>}
        {activity.locallyConstrained && (
          <span className="route route-info" title="The verifier added its ACI routing policy (provider.aci_verified, pinned sessions) and re-serialized the body; the receipt binds the bytes sent to the service, not the agent's original request.">
            ACI policy applied locally
          </span>
        )}
        {activity.rewritten && (
          <span className="route route-warning" title="The service rewrote the request before inference; the receipt records that rewrite.">
            Rewritten by service
          </span>
        )}
        {(activity.status < 200 || activity.status >= 300) && <span>HTTP {activity.status}</span>}
        {activity.streamed && <span>Streamed</span>}
        <time>{formatTimestamp(activity.at * 1_000)}</time>
      </div>
    </article>
  );
}

function IdentityDetails({ state }: { state: GatewayState }): React.JSX.Element | null {
  const identity = state.identity;
  if (!identity) {
    return null;
  }
  return (
    <>
      <div className="identity-grid">
        <Detail label="Hardware" value={hardwareName(identity.teeType)} />
        <Detail label="Trust" value={trustName(identity.trustLevel)} />
        <Detail
          label="Source"
          value={identity.source.repoCommit ? shorten(identity.source.repoCommit, 11) : "Unknown"}
          mono
        />
        <Detail label="Valid until" value={formatTimestamp(identity.keysetNotAfter * 1_000, true)} />
        <Detail label="Serving" value={identity.serving} />
        <Detail label="E2EE" value={identity.supportedE2eeVersions.join(", ") || "None"} mono />
        <Detail label="Keyset digest" value={identity.keysetDigest} mono wide />
        {identity.tlsSpki && <Detail label="TLS key" value={identity.tlsSpki} mono wide />}
      </div>
      <div className="check-list">
        {state.checks.map((check) => <CheckRow key={check.id} check={check} />)}
      </div>
    </>
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
    <div className="check-row" title={`${check.title}: ${check.detail}`}>
      <span className={`check-icon check-${check.status}`}>
        {check.status === "pass" && <Check size={12} />}
      </span>
      <div className="check-copy">
        <strong>{title}</strong>
      </div>
      <span className={`result result-${check.status}`}>{checkStatusLabel(check.status)}</span>
    </div>
  );
}

function EmptyState({ text }: { text: string }): React.JSX.Element {
  return <div className="empty-state">{text}</div>;
}

// Badge label, plain-language verdict line, and icon for the gateway state.
function presentation(state: GatewayState): {
  label: string;
  summary: string;
  tone: "success" | "warning" | "danger" | "neutral";
  icon: typeof ShieldCheck;
} {
  if (state.endpointError) {
    return { label: "Endpoint busy", summary: "The local endpoint could not be claimed - free port 4180 and relaunch", tone: "danger", icon: ShieldX };
  }
  switch (state.status) {
    case "verifying":
      return { label: "Verifying...", summary: state.progress ?? "Verifying the service...", tone: "warning", icon: LoaderCircle };
    case "blocked":
      return { label: "Blocked", summary: "Requests blocked - service identity changed", tone: "danger", icon: ShieldX };
    case "error":
      return { label: "Failed", summary: "Verification failed - requests are blocked", tone: "danger", icon: ShieldX };
    case "stopped":
      return { label: "Stopped", summary: "Start to verify the service", tone: "neutral", icon: CircleStop };
    case "verified":
      if (!state.apiKeySaved) {
        return { label: "Key needed", summary: "Service verified - save your API key to send requests", tone: "warning", icon: ShieldCheck };
      }
      return { label: "Ready", summary: "Service verified - requests protected", tone: "success", icon: ShieldCheck };
  }
}

function auditPresentation(activity: RequestActivity): { label: string; tone: string } {
  if (activity.verified === true) {
    return { label: "Receipt verified", tone: "verified" };
  }
  if (activity.verified === false) {
    return { label: "Receipt failed", tone: "failed" };
  }
  if (activity.receiptId) {
    return { label: "Receipt pending", tone: "unverified" };
  }
  return activity.status >= 400
    ? { label: "Rejected locally", tone: "failed" }
    : { label: "No receipt", tone: "neutral" };
}

function routePresentation(activity: RequestActivity): { label: string; tone: string; title: string } | undefined {
  switch (activity.route) {
    case "declared":
      return { label: "Declared surface", tone: "success", title: "The service declares this surface for every catalog model; helper endpoints are gated the same way. The receipt binds the bytes the verifier sent" };
    default:
      return undefined;
  }
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

function remoteHost(value: string): string | undefined {
  try {
    return new URL(value).host;
  } catch {
    return undefined;
  }
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
    : { hour: "2-digit", minute: "2-digit", second: "2-digit" };
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
