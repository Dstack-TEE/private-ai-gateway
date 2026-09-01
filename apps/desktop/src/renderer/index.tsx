import React, { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Activity,
  Check,
  CheckCircle2,
  CircleStop,
  Clipboard,
  LoaderCircle,
  Play,
  ShieldCheck,
  ShieldX,
} from "lucide-react";

import { desktopApi } from "./desktop-api";
import type {
  GatewayState,
  RequestActivity,
  VerificationCheck,
} from "../shared/contracts";
import "./styles.css";

const DEFAULT_REMOTE_URL = "https://tee.redpill.ai";
const INITIAL_STATE: GatewayState = { status: "stopped", checks: [], activity: [] };

type AuditTab = "requests" | "checks";

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

function App(): React.JSX.Element {
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(DEFAULT_REMOTE_URL);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [activeTab, setActiveTab] = useState<AuditTab>("requests");
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const busy = state.status === "verifying";
  const running = state.status === "verified" || state.status === "blocked";

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

  const start = async () => {
    setActionError(undefined);
    try {
      setState(await desktopApi.start({ remoteUrl, requireProductionOs }));
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const stop = async () => {
    setActionError(undefined);
    try {
      setState(await desktopApi.stop());
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const copy = async (label: string, value: string) => {
    setActionError(undefined);
    try {
      await desktopApi.copyText(value);
      setCopied(label);
      window.setTimeout(
        () => setCopied((current) => current === label ? undefined : current),
        1_400,
      );
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const status = useMemo(() => statusPresentation(state.status), [state.status]);
  const StatusIcon = status.icon;

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-mark" aria-hidden="true"><ShieldCheck size={18} /></div>
        <div className="brand-copy">
          <h1>Private AI Gateway</h1>
          <span>Verified endpoint for coding agents</span>
        </div>
        <div className={`status-badge status-${state.status}`}>
          <StatusIcon size={14} className={busy ? "spin" : undefined} />
          {status.label}
        </div>
      </header>

      <section className="connection-section" aria-label="AI service connection">
        <label className="url-field">
          <span>AI service</span>
          <input
            value={remoteUrl}
            onChange={(event) => setRemoteUrl(event.target.value)}
            disabled={busy || running}
            spellCheck={false}
          />
        </label>
        <div className="connection-actions">
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
          {running ? (
            <button className="command-button stop-button" onClick={() => void stop()}>
              <CircleStop size={15} /> Stop
            </button>
          ) : (
            <button className="command-button start-button" onClick={() => void start()} disabled={busy}>
              {busy ? <LoaderCircle className="spin" size={15} /> : <Play size={15} />}
              {busy ? "Verifying..." : "Start"}
            </button>
          )}
        </div>
      </section>

      {(actionError || state.error) && (
        <div className="error-banner" role="alert">
          <ShieldX size={16} />
          <span>{actionError ?? state.error}</span>
        </div>
      )}

      <section className="endpoint-section" aria-label="Coding agent endpoints">
        <SectionHeading title="Point your agent here" meta={state.proxyUrl ? "Ready" : "Off"} />
        <EndpointRow label="OpenAI" value={openAiEndpoint(state.proxyUrl)} copied={copied} onCopy={copy} />
        <EndpointRow label="Anthropic" value={state.proxyUrl} copied={copied} onCopy={copy} />
      </section>

      <section className="identity-section" aria-label="Verified service identity">
        <SectionHeading title="Verified service" meta={remoteHost(state.remoteUrl ?? remoteUrl)} />
        {state.identity
          ? <IdentitySummary state={state} />
          : <EmptyState text={busy ? "Verifying the service..." : "Start to verify the service"} />}
      </section>

      <section className="audit-section">
        <div className="tabs" role="tablist" aria-label="Gateway audit data">
          <button
            id="requests-tab"
            role="tab"
            aria-selected={activeTab === "requests"}
            aria-controls="audit-panel"
            className={activeTab === "requests" ? "active" : undefined}
            onClick={() => setActiveTab("requests")}
          >
            <Activity size={14} /> Requests <span>{state.activity.length}</span>
          </button>
          <button
            id="checks-tab"
            role="tab"
            aria-selected={activeTab === "checks"}
            aria-controls="audit-panel"
            className={activeTab === "checks" ? "active" : undefined}
            onClick={() => setActiveTab("checks")}
          >
            <ShieldCheck size={14} /> Checks <span>{checkCount(state.checks)}</span>
          </button>
        </div>

        <div
          id="audit-panel"
          className="audit-content"
          role="tabpanel"
          aria-labelledby={`${activeTab}-tab`}
          tabIndex={0}
        >
          {activeTab === "checks" ? (
            state.checks.length > 0 ? (
              <div className="check-list">
                {state.checks.map((check) => <CheckRow key={check.id} check={check} />)}
              </div>
            ) : (
              <EmptyState text={busy ? "Running service checks..." : "Start to run checks"} />
            )
          ) : (
            <RequestAudit activity={state.activity} running={running} />
          )}
        </div>
      </section>
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

function IdentitySummary({ state }: { state: GatewayState }): React.JSX.Element {
  const identity = state.identity;
  if (!identity) {
    return <EmptyState text="Start to verify the service" />;
  }
  return (
    <div className="identity-summary">
      <div className="identity-grid">
        <Detail label="Hardware" value={hardwareName(identity.teeType)} />
        <Detail label="Trust" value={trustName(identity.trustLevel)} />
        <Detail
          label="Source"
          value={identity.source.repoCommit ? shorten(identity.source.repoCommit, 11) : "Unknown"}
          mono
        />
        <Detail label="Valid until" value={formatTimestamp(identity.keysetNotAfter * 1_000, true)} />
      </div>
      <details className="technical-details">
        <summary>Technical details</summary>
        <div>
          <Detail label="Serving" value={identity.serving} />
          <Detail label="E2EE" value={identity.supportedE2eeVersions.join(", ") || "None"} mono />
          <Detail label="Keyset digest" value={identity.keysetDigest} mono wide />
        </div>
      </details>
    </div>
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
        text={running ? "No requests yet - point an agent at the endpoint above" : "Start the gateway to see requests"}
      />
    );
  }
  return (
    <div className="request-audit">
      {activity.map((item, index) => (
        <ActivityRow key={item.receiptId ?? `${item.at}-${item.path}-${index}`} activity={item} />
      ))}
    </div>
  );
}

function ActivityRow({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const audit = auditStatus(activity);
  return (
    <article className="audit-row" title={activity.detail}>
      <div className="audit-mainline">
        <span className="method">{activity.method}</span>
        <code title={activity.path}>{activity.path}</code>
        <span className={`audit-state audit-${audit.tone}`}>{audit.label}</span>
      </div>
      <div className="audit-meta">
        {activity.receiptId && <code title={activity.receiptId}>{shorten(activity.receiptId, 24)}</code>}
        {(activity.status < 200 || activity.status >= 300) && <span>HTTP {activity.status}</span>}
        {activity.streamed && <span>Streamed</span>}
        <time>{formatTimestamp(activity.at * 1_000)}</time>
      </div>
    </article>
  );
}

function EmptyState({ text }: { text: string }): React.JSX.Element {
  return <div className="empty-state">{text}</div>;
}

function statusPresentation(status: GatewayState["status"]): {
  label: string;
  icon: typeof ShieldCheck;
} {
  switch (status) {
    case "verifying": return { label: "Verifying...", icon: LoaderCircle };
    case "verified": return { label: "Verified", icon: ShieldCheck };
    case "blocked": return { label: "Blocked", icon: ShieldX };
    case "error": return { label: "Failed", icon: ShieldX };
    case "stopped": return { label: "Off", icon: CircleStop };
  }
}

function auditStatus(activity: RequestActivity): {
  label: "Verified" | "Failed" | "Unverified" | "No receipt";
  tone: "verified" | "failed" | "unverified" | "neutral";
} {
  if (activity.verified === true) {
    return { label: "Verified", tone: "verified" };
  }
  if (activity.verified === false) {
    return { label: "Failed", tone: "failed" };
  }
  return activity.receiptId
    ? { label: "Unverified", tone: "unverified" }
    : { label: "No receipt", tone: "neutral" };
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
