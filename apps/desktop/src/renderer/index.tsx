import React, { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Check,
  CheckCircle2,
  CircleStop,
  Clipboard,
  LoaderCircle,
  MessageSquareText,
  Play,
  RefreshCw,
  ShieldCheck,
  ShieldX,
} from "lucide-react";

import { desktopApi } from "./desktop-api";
import type {
  GatewayState,
  ReceiptSummary,
  RequestActivity,
  VerificationCheck,
} from "../shared/contracts";
import "./styles.css";

const DEFAULT_REMOTE_URL = "https://tee.redpill.ai";
const INITIAL_STATE: GatewayState = { status: "stopped", checks: [], activity: [] };

type AuditTab = "verification" | "messages";

function App(): React.JSX.Element {
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(DEFAULT_REMOTE_URL);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [receipts, setReceipts] = useState<ReceiptSummary[]>([]);
  const [activeTab, setActiveTab] = useState<AuditTab>("verification");
  const [actionError, setActionError] = useState<string>();
  const [copied, setCopied] = useState<string>();
  const busy = state.status === "verifying";
  const running = state.status === "verified" || state.status === "blocked";

  const refreshReceipts = useCallback(async () => {
    try {
      setReceipts(await desktopApi.listReceipts());
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }, []);

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

  useEffect(() => {
    if (!running) {
      setReceipts([]);
      return undefined;
    }
    void refreshReceipts();
    const timer = window.setInterval(() => void refreshReceipts(), 3_000);
    return () => window.clearInterval(timer);
  }, [refreshReceipts, running]);

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
          <span>Local ACI proxy</span>
        </div>
        <div className={`status-badge status-${state.status}`}>
          <StatusIcon size={14} className={busy ? "spin" : undefined} />
          {status.label}
        </div>
      </header>

      <section className="connection-section" aria-label="Gateway connection">
        <label className="url-field">
          <span>Gateway URL</span>
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
            Production OS
          </label>
          {running ? (
            <button className="command-button stop-button" onClick={() => void stop()}>
              <CircleStop size={15} /> Stop
            </button>
          ) : (
            <button className="command-button start-button" onClick={() => void start()} disabled={busy}>
              {busy ? <LoaderCircle className="spin" size={15} /> : <Play size={15} />}
              {busy ? "Verifying" : "Start"}
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

      <section className="endpoint-section" aria-label="Local endpoints">
        <SectionHeading title="Local endpoints" meta={state.proxyUrl ? "Ready" : "Unavailable"} />
        <EndpointRow label="OpenAI" value={state.proxyUrl} copied={copied} onCopy={copy} />
        <EndpointRow label="Anthropic" value={state.proxyUrl} copied={copied} onCopy={copy} />
      </section>

      <section className="identity-section" aria-label="ACI identity">
        <SectionHeading title="ACI identity" meta={state.identity?.teeType.toUpperCase()} />
        {state.identity
          ? <IdentitySummary state={state} />
          : <EmptyState text={busy ? "Verification in progress" : "Not verified"} />}
      </section>

      <section className="audit-section">
        <div className="tabs" role="tablist" aria-label="Gateway audit data">
          <button
            role="tab"
            aria-selected={activeTab === "verification"}
            className={activeTab === "verification" ? "active" : undefined}
            onClick={() => setActiveTab("verification")}
          >
            <ShieldCheck size={14} /> Verification <span>{state.checks.length}</span>
          </button>
          <button
            role="tab"
            aria-selected={activeTab === "messages"}
            className={activeTab === "messages" ? "active" : undefined}
            onClick={() => setActiveTab("messages")}
          >
            <MessageSquareText size={14} /> Messages <span>{state.activity.length}</span>
          </button>
          {activeTab === "messages" && (
            <button
              className="refresh-button"
              onClick={() => void refreshReceipts()}
              disabled={!running}
              aria-label="Refresh receipt records"
              title="Refresh receipt records"
            >
              <RefreshCw size={14} />
            </button>
          )}
        </div>

        <div className="audit-content" role="tabpanel">
          {activeTab === "verification" ? (
            state.checks.length > 0 ? (
              <div className="check-list">
                {state.checks.map((check) => <CheckRow key={check.id} check={check} />)}
              </div>
            ) : (
              <EmptyState text={busy ? "Verification in progress" : "No verification run"} />
            )
          ) : (
            <MessageAudit activity={state.activity} receipts={receipts} running={running} />
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
      <code title={value}>{value ?? "Unavailable"}</code>
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
    return <EmptyState text="Not verified" />;
  }
  return (
    <div className="identity-grid">
      <Detail label="Trust" value={identity.trustLevel.replaceAll("_", " ")} />
      <Detail label="Serving" value={identity.serving} />
      <Detail
        label="Source"
        value={identity.source.repoCommit ? shorten(identity.source.repoCommit, 13) : "Unknown"}
        mono
      />
      <Detail label="Expires" value={formatTimestamp(identity.keysetNotAfter * 1_000, true)} />
      <Detail label="Keyset" value={identity.keysetDigest} mono wide />
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
  return (
    <div className="check-row">
      <span className={`check-icon check-${check.status}`}>
        {check.status === "pass" && <Check size={12} />}
      </span>
      <div className="check-copy">
        <div><strong>{check.title}</strong><code>{check.id}</code></div>
        <p title={check.detail}>{check.detail}</p>
      </div>
      <span className={`result result-${check.status}`}>{check.status}</span>
    </div>
  );
}

function MessageAudit({
  activity,
  receipts,
  running,
}: {
  activity: RequestActivity[];
  receipts: ReceiptSummary[];
  running: boolean;
}): React.JSX.Element {
  if (activity.length === 0 && receipts.length === 0) {
    return <EmptyState text={running ? "No message activity yet" : "Gateway stopped"} />;
  }
  return (
    <div className="message-audit">
      {activity.length > 0 && (
        <div className="audit-group">
          <h3>Request events</h3>
          {activity.map((item, index) => (
            <ActivityRow key={`${item.at}-${item.path}-${index}`} activity={item} />
          ))}
        </div>
      )}
      {receipts.length > 0 && (
        <div className="audit-group">
          <h3>Receipt records</h3>
          {receipts.map((receipt) => <ReceiptRow key={receipt.receiptId} receipt={receipt} />)}
        </div>
      )}
    </div>
  );
}

function ActivityRow({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const audit = auditStatus(activity.verified);
  return (
    <article className="audit-row">
      <div className="audit-mainline">
        <span className="method">{activity.method}</span>
        <code title={activity.path}>{activity.path}</code>
        <span className={`audit-state audit-${audit}`}>{audit}</span>
      </div>
      <p title={activity.detail}>{activity.detail || "Request completed"}</p>
      <div className="audit-meta">
        <span>HTTP {activity.status}</span>
        <span>{activity.streamed ? "Stream" : "Buffered"}</span>
        <time>{formatTimestamp(activity.at * 1_000)}</time>
      </div>
    </article>
  );
}

function ReceiptRow({ receipt }: { receipt: ReceiptSummary }): React.JSX.Element {
  const audit = auditStatus(receipt.verified);
  return (
    <article className="audit-row">
      <div className="audit-mainline">
        <code title={receipt.receiptId}>{shorten(receipt.receiptId, 22)}</code>
        <span className={`audit-state audit-${audit}`}>{audit}</span>
      </div>
      <p title={receipt.path}>{receipt.path}</p>
      <div className="audit-meta">
        <span>HTTP {receipt.status}</span>
        <span>{receipt.streamed ? "Stream" : "Buffered"}</span>
        {receipt.truncated && <span className="danger-text">Truncated</span>}
        <time>{formatTimestamp(receipt.at * 1_000)}</time>
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
    case "verifying": return { label: "Verifying", icon: LoaderCircle };
    case "verified": return { label: "Verified", icon: ShieldCheck };
    case "blocked": return { label: "Blocked", icon: ShieldX };
    case "error": return { label: "Error", icon: ShieldX };
    case "stopped": return { label: "Stopped", icon: CircleStop };
  }
}

function auditStatus(value: boolean | null): "verified" | "failed" | "recorded" {
  return value === true ? "verified" : value === false ? "failed" : "recorded";
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
  if (error instanceof Error) {
    return error.message;
  }
  return typeof error === "string" && error.length > 0 ? error : "Unexpected desktop error";
}

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing renderer root");
}
createRoot(root).render(<App />);
