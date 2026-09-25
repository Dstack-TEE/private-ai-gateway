import React from "react";
import { Check, LockOpen, RefreshCw, ShieldCheck, ShieldX } from "lucide-react";
import { AppDialog, DoneFooter } from "../components/app-dialog";
import type { AppState, VerificationCheck } from "../../shared/contracts";
import { hasLiveVerification } from "../lib/protection";
import { formatTimestamp, hardwareName, shorten, trustName } from "../lib/format";
import { Detail } from "../components/detail";
import { VerificationVerdict } from "../components/verification-verdict";
import { cn } from "../lib/utils";
import { toneTextClass, type Tone } from "../lib/tone";

const CHECK_ICON_CLASS = "grid size-4.5 flex-none place-items-center rounded-full";
const CHECK_PRESENTATION: Record<VerificationCheck["status"], { iconClass: string; tone: Tone }> = {
  pass: { iconClass: "bg-primary text-primary-foreground", tone: "success" },
  fail: { iconClass: "bg-[var(--danger-bg)] text-destructive", tone: "danger" },
  skip: { iconClass: "bg-[var(--warning-bg)] text-warning", tone: "warning" },
  info: { iconClass: "bg-[var(--warning-bg)] text-warning", tone: "warning" },
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

export function PrivacyDialog({ state, onClose }: { state: AppState; onClose(): void }): React.JSX.Element {
  return <AppDialog title="Privacy verification" className="sm:max-w-2xl" onClose={onClose}>
    <div className="-mx-6 min-h-0 overflow-y-auto px-6"><PrivacyVerification state={state} /></div>
    <DoneFooter />
  </AppDialog>;
}

function PrivacyVerification({ state }: { state: AppState }): React.JSX.Element {
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
        ? "Requests leave this device only over an SPKI-pinned TLS channel whose key is bound to the verified service identity."
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
  const verdictTone = verified ? "success" : state.status === "blocked" || state.status === "error" ? "danger" : "neutral";
  const VerdictIcon = verified ? ShieldCheck : state.status === "verifying" ? RefreshCw : ShieldX;
  return (
    <section aria-label="Privacy">
      <VerificationVerdict
        tone={verdictTone}
        icon={VerdictIcon}
        title={verified ? "Service identity and connection verified" : state.status === "verifying" ? "Checking the service" : "No verified live connection"}
        detail={verified ? "This app checked the service's hardware evidence and bound the encrypted connection to its attested key." : "A saved profile is not evidence of a currently protected connection. Protection must establish a new verified session."}
      />
      <div className="privacy-facts mt-4">
        {facts.map((fact) => (
          <div className="fact flex min-h-12.5 items-start gap-3 border-b border-border p-3.5 last:border-b-0" key={fact.title}>
            <span className={cn(CHECK_ICON_CLASS, CHECK_PRESENTATION[fact.ok ? "pass" : "skip"].iconClass)} aria-hidden="true">
              {fact.ok ? <Check size={12} /> : <LockOpen size={11} />}
            </span>
            <span className="grid min-w-0 flex-auto gap-1.25">
              <span className="font-medium">{fact.title}</span>
              <span className="text-xs text-muted-foreground wrap-anywhere">{fact.detail}</span>
            </span>
          </div>
        ))}
      </div>
      <p className="proof-boundary mt-3 mr-0 mb-4.5 ml-0 text-muted-foreground text-xs">{passed("id-5") ? "Key custody evidence passed." : "Key custody is not independently established by these checks."} Responses are forwarded immediately; receipts are audited afterward and cannot retract delivered content. This summary does not verify upstream inference or answer accuracy. {!state.config.requireProductionOs && "Development OS images are allowed."}</p>
      {identity && (
        <section className="privacy-section mt-6" aria-labelledby="verified-identity-title">
          <SectionHeading id="verified-identity-title" title={verified ? "Current service identity" : "Last reported identity"} summary={`${checkCount(checks)} checks passed`} />
          <div className="identity-grid grid grid-cols-2 gap-x-5 gap-y-4 p-3.5 [&_.wide]:col-span-full [&_>_div]:min-w-0 [&_span]:mb-0.5 [&_span]:block [&_span]:text-xs [&_span]:text-muted-foreground [&_strong]:block [&_strong]:select-text [&_strong]:font-semibold [&_strong.mono]:whitespace-normal [&_strong.mono]:font-medium [&_strong.mono]:wrap-anywhere">
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
        <section className="privacy-section mt-6" aria-labelledby="verification-checks-title">
          <SectionHeading id="verification-checks-title" title="Verification checks" summary={`${checks.length} total`} />
          <div className="check-list">{checks.map((check) => <CheckRow key={check.id} check={check} />)}</div>
        </section>
      )}
    </section>
  );
}

function CheckRow({ check }: { check: VerificationCheck }): React.JSX.Element {
  const title = CHECK_TITLES[check.id] ?? check.title;
  const presentation = CHECK_PRESENTATION[check.status];
  return (
    <div className="check-row grid min-h-9 grid-cols-[18px_minmax(0,_1fr)_auto] items-start gap-3 border-b border-border px-3.5 py-3 last:border-b-0">
      <span className={cn(CHECK_ICON_CLASS, presentation.iconClass)} aria-hidden="true">
        {check.status === "pass" && <Check size={12} />}
      </span>
      <span className="grid min-w-0 gap-1.25"><span className="font-medium">{title}</span><span className="select-text text-xs text-muted-foreground wrap-anywhere">{check.detail}</span></span>
      <span className={cn("flex-none text-xs font-semibold", toneTextClass[presentation.tone])}>{checkStatusLabel(check.status)}</span>
    </div>
  );
}

function SectionHeading({ id, title, summary }: { id: string; title: string; summary: string }): React.JSX.Element {
  return <div className="flex min-h-9 flex-wrap items-center justify-between gap-x-4 gap-y-1 px-0.5 pb-2"><h3 id={id} className="text-sm font-semibold text-foreground">{title}</h3><span className="text-xs text-muted-foreground">{summary}</span></div>;
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
