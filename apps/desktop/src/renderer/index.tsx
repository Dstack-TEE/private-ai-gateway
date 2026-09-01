import React, { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Check,
  CircleStop,
  Clipboard,
  LoaderCircle,
  Play,
  RefreshCw,
  ShieldCheck,
  ShieldX,
} from "lucide-react";

import type {
  GatewayState,
  ReceiptSummary,
  VerificationCheck,
} from "../shared/contracts";
import "./styles.css";

const DEFAULT_REMOTE_URL = "https://tee.redpill.ai";
const INITIAL_STATE: GatewayState = { status: "stopped", checks: [], activity: [] };

function App(): React.JSX.Element {
  const [state, setState] = useState<GatewayState>(INITIAL_STATE);
  const [remoteUrl, setRemoteUrl] = useState(DEFAULT_REMOTE_URL);
  const [requireProductionOs, setRequireProductionOs] = useState(false);
  const [receipts, setReceipts] = useState<ReceiptSummary[]>([]);
  const [actionError, setActionError] = useState<string>();
  const busy = state.status === "verifying";
  const running = state.status === "verified" || state.status === "blocked";

  const refreshReceipts = useCallback(async () => {
    try {
      setReceipts(await window.privateAiGateway.listReceipts());
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }, []);

  useEffect(() => {
    void window.privateAiGateway.getState().then(setState).catch((error) => {
      setActionError(errorMessage(error));
    });
    return window.privateAiGateway.onStateChange(setState);
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
      setState(await window.privateAiGateway.start({ remoteUrl, requireProductionOs }));
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const stop = async () => {
    setActionError(undefined);
    try {
      setState(await window.privateAiGateway.stop());
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };

  const status = useMemo(() => statusPresentation(state.status), [state.status]);
  const StatusIcon = status.icon;

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-mark" aria-hidden="true"><ShieldCheck size={20} /></div>
        <div className="brand-copy">
          <h1>Private AI Gateway</h1>
          <span>ACI desktop controller</span>
        </div>
        <div className={`status-badge status-${state.status}`}>
          <StatusIcon size={15} className={busy ? "spin" : undefined} />
          {status.label}
        </div>
      </header>

      <section className="connection-bar" aria-label="Gateway connection">
        <label className="url-field">
          <span>Gateway URL</span>
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
          Production OS
        </label>
        {running ? (
          <button className="button button-stop" onClick={() => void stop()}>
            <CircleStop size={17} /> Stop
          </button>
        ) : (
          <button className="button button-start" onClick={() => void start()} disabled={busy}>
            {busy ? <LoaderCircle className="spin" size={17} /> : <Play size={17} />}
            {busy ? "Verifying" : "Start"}
          </button>
        )}
      </section>

      {(actionError || state.error) && (
        <div className="error-banner" role="alert">
          <ShieldX size={18} />
          <span>{actionError ?? state.error}</span>
        </div>
      )}

      <div className="workspace-grid">
        <div className="primary-column">
          <section className="panel endpoints-panel">
            <PanelHeading title="Local endpoints" meta={state.identity?.serving} />
            <EndpointRow label="OpenAI compatible" value={state.proxyUrl} />
            <EndpointRow label="Anthropic compatible" value={state.proxyUrl} />
          </section>

          <section className="panel identity-panel">
            <PanelHeading title="ACI identity" meta={state.identity?.teeType.toUpperCase()} />
            {state.identity ? <IdentityDetails state={state} /> : <EmptyState text="Not verified" />}
          </section>

          <section className="panel checks-panel">
            <PanelHeading title="Verification" meta={`${state.checks.length} checks`} />
            {state.checks.length > 0 ? (
              <div className="checks-list">
                {state.checks.map((check) => <CheckRow key={check.id} check={check} />)}
              </div>
            ) : (
              <EmptyState text={busy ? "Verification in progress" : "No verification run"} />
            )}
          </section>
        </div>

        <aside className="panel receipts-panel">
          <PanelHeading
            title="Receipts"
            meta={`${receipts.length} recent`}
            action={
              <button
                className="icon-button"
                onClick={() => void refreshReceipts()}
                disabled={!running}
                aria-label="Refresh receipts"
                title="Refresh receipts"
              >
                <RefreshCw size={16} />
              </button>
            }
          />
          {receipts.length > 0 ? (
            <div className="receipt-list">
              {receipts.map((receipt) => <ReceiptRow key={receipt.receiptId} receipt={receipt} />)}
            </div>
          ) : (
            <EmptyState text={running ? "No receipts yet" : "Gateway stopped"} />
          )}
        </aside>
      </div>
    </main>
  );
}

function PanelHeading({
  title,
  meta,
  action,
}: {
  title: string;
  meta?: string;
  action?: React.ReactNode;
}): React.JSX.Element {
  return (
    <div className="panel-heading">
      <h2>{title}</h2>
      <div className="panel-heading-meta">{meta && <span>{meta}</span>}{action}</div>
    </div>
  );
}

function EndpointRow({ label, value }: { label: string; value?: string }): React.JSX.Element {
  return (
    <div className="endpoint-row">
      <span>{label}</span>
      <code>{value ?? "Unavailable"}</code>
      <button
        className="icon-button"
        disabled={!value}
        onClick={() => value && void window.privateAiGateway.copyText(value)}
        aria-label={`Copy ${label} endpoint`}
        title={`Copy ${label} endpoint`}
      >
        <Clipboard size={15} />
      </button>
    </div>
  );
}

function IdentityDetails({ state }: { state: GatewayState }): React.JSX.Element {
  const identity = state.identity;
  if (!identity) {
    return <EmptyState text="Not verified" />;
  }
  return (
    <dl className="identity-grid">
      <Detail label="Trust" value={identity.trustLevel.replaceAll("_", " ")} />
      <Detail label="Keyset expires" value={formatTimestamp(identity.keysetNotAfter * 1_000)} />
      <Detail label="Source commit" value={identity.source.repoCommit ?? "Unknown"} mono />
      <Detail label="Keyset digest" value={identity.keysetDigest} mono wide />
      <Detail label="TLS SPKI" value={identity.tlsSpki ?? "Not reported"} mono wide />
    </dl>
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
    <div className={wide ? "detail-wide" : undefined}>
      <dt>{label}</dt>
      <dd className={mono ? "mono" : undefined} title={value}>{value}</dd>
    </div>
  );
}

function CheckRow({ check }: { check: VerificationCheck }): React.JSX.Element {
  return (
    <div className="check-row">
      <span className={`check-icon check-${check.status}`}>
        {check.status === "pass" ? <Check size={14} /> : <span />}
      </span>
      <div>
        <div className="check-title"><code>{check.id}</code><strong>{check.title}</strong></div>
        <p>{check.detail}</p>
      </div>
      <span className={`check-status-text check-${check.status}`}>{check.status}</span>
    </div>
  );
}

function ReceiptRow({ receipt }: { receipt: ReceiptSummary }): React.JSX.Element {
  const audit = receipt.verified === true ? "verified" : receipt.verified === false ? "failed" : "recorded";
  return (
    <article className="receipt-row">
      <div className="receipt-topline">
        <code>{shorten(receipt.receiptId)}</code>
        <span className={`audit audit-${audit}`}>{audit}</span>
      </div>
      <div className="receipt-path">{receipt.path}</div>
      <div className="receipt-meta">
        <span>HTTP {receipt.status}</span>
        <span>{receipt.streamed ? "stream" : "buffered"}</span>
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

function shorten(value: string): string {
  return value.length > 18 ? `${value.slice(0, 9)}...${value.slice(-6)}` : value;
}

function formatTimestamp(value: number): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "Unexpected desktop error";
}

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing renderer root");
}
createRoot(root).render(<App />);
