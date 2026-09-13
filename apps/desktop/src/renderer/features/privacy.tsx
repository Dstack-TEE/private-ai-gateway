import React from "react";
import { Check, LockOpen, ShieldCheck, ShieldX } from "lucide-react";
import { Sheet, DismissSheetAction } from "../components/sheet";
import type { GatewayState, VerificationCheck } from "../../shared/contracts";
import { hasLiveVerification } from "../lib/protection";
import { formatTimestamp, hardwareName, shorten, trustName } from "../lib/format";
import { Detail } from "../components/detail";

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

export function PrivacyVerificationSheet({ state, onClose }: { state: GatewayState; onClose(): void }): React.JSX.Element {
  return <Sheet title="Privacy verification" className="privacy-sheet w-[min(680px,_calc(var(--window-dialog-width,_100vw)_-_32px))] h-[min(680px,_calc(var(--window-dialog-height,_100vh)_-_32px))]" onClose={onClose}><PrivacyVerification state={state} /><DismissSheetAction onClose={onClose} /></Sheet>;
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
    <section className="privacy-content mt-3.5" aria-label="Privacy">
      <div className={`[&.state-success]:text-primary [&.state-neutral]:text-muted-foreground [&.state-warning]:text-warning [&.state-danger]:text-destructive privacy-verdict [&.state-neutral]:bg-transparent [&.state-neutral]:border-border [&.state-danger]:bg-transparent [&.state-danger]:border-current p-3.5 flex items-start gap-3 bg-muted border border-border rounded-2xl [&.state-success]:bg-primary/10 [&.state-success]:border-[color-mix(in_srgb,_var(--primary)_18%,_transparent)] [&_>_svg]:flex-none [&_>_span]:min-w-0 [&_>_span]:grid [&_>_span]:gap-1.5 [&_>_span]:wrap-anywhere [&_strong]:text-foreground [&_small]:text-muted-foreground [&_small]:text-xs state-${verified ? "success" : state.status === "blocked" || state.status === "error" ? "danger" : "neutral"}`}>
        {verified ? <ShieldCheck size={22} aria-hidden="true" /> : <ShieldX size={22} aria-hidden="true" />}
        <span><strong>{verified ? "Service identity and connection verified" : state.status === "verifying" ? "Checking the service" : "No verified live connection"}</strong><small>{verified ? "This app checked the service's hardware evidence and bound the encrypted connection to its attested key." : "A saved profile is not evidence of a currently protected connection. Protection must establish a new verified session."}</small></span>
      </div>
      <div className="sheet-card privacy-facts mt-4 [&_.row]:p-3.5 [&_.row-main]:grid [&_.row-main]:gap-1.25">
        {facts.map((fact) => (
          <div className="row min-h-12.5 pt-2.25 pr-3 pb-2.25 pl-3 flex items-center gap-3 border-b border-b-border last:border-b-0 fact items-start" key={fact.title}>
            <span className={fact.ok ? "check-icon w-4.5 h-4.5 flex-none grid place-items-center rounded-full [&.check-pass]:text-primary-foreground [&.check-pass]:bg-primary [&.check-fail]:text-destructive [&.check-fail]:bg-[var(--danger-bg)] [&.check-skip]:text-warning [&.check-skip]:bg-[var(--warning-bg)] [&.check-info]:text-warning [&.check-info]:bg-[var(--warning-bg)] check-pass" : "check-icon w-4.5 h-4.5 flex-none grid place-items-center rounded-full [&.check-pass]:text-primary-foreground [&.check-pass]:bg-primary [&.check-fail]:text-destructive [&.check-fail]:bg-[var(--danger-bg)] [&.check-skip]:text-warning [&.check-skip]:bg-[var(--warning-bg)] [&.check-info]:text-warning [&.check-info]:bg-[var(--warning-bg)] check-skip"} aria-hidden="true">
              {fact.ok ? <Check size={12} /> : <LockOpen size={11} />}
            </span>
            <span className="row-main min-w-0 flex-auto flex flex-wrap items-center gap-y-0.5 gap-x-2">
              {fact.title}
              <span className="row-note flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap">{fact.detail}</span>
            </span>
          </div>
        ))}
      </div>
      <p className="proof-boundary mt-3 mr-0 mb-4.5 ml-0 text-muted-foreground text-xs">{passed("id-5") ? "Key custody evidence passed." : "Key custody is not independently established by these checks."} This summary does not verify upstream inference or answer accuracy. {!state.config.requireProductionOs && "Development OS images are allowed."}</p>
      {identity && (
        <section className="privacy-section mt-6" aria-labelledby="verified-identity-title">
          <div className="privacy-section-heading min-h-9 pt-0 pr-0.5 pb-2 pl-0.5 flex items-center flex-wrap justify-between gap-y-1 gap-x-4 [&_h3]:m-0 [&_h3]:text-foreground [&_h3]:text-sm [&_h3]:font-semibold [&_>_span]:text-muted-foreground [&_>_span]:text-xs"><h3 id="verified-identity-title">{verified ? "Current service identity" : "Last reported identity"}</h3><span>{checkCount(checks)} checks passed</span></div>
          <div className="sheet-card identity-grid [&_strong]:select-text p-3.5 grid grid-cols-2 gap-y-4 gap-x-5 border-t-border [&_>_div]:min-w-0 [&_.wide]:col-span-full [&_span]:block [&_span]:mb-0.5 [&_span]:text-muted-foreground [&_span]:text-xs [&_strong]:block [&_strong]:font-semibold [&_strong.mono]:font-medium [&_strong.mono]:wrap-anywhere [&_strong.mono]:whitespace-normal border-t-0">
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
          <div className="privacy-section-heading min-h-9 pt-0 pr-0.5 pb-2 pl-0.5 flex items-center flex-wrap justify-between gap-y-1 gap-x-4 [&_h3]:m-0 [&_h3]:text-foreground [&_h3]:text-sm [&_h3]:font-semibold [&_>_span]:text-muted-foreground [&_>_span]:text-xs"><h3 id="verification-checks-title">Verification checks</h3><span>{checks.length} total</span></div>
          <div className="sheet-card check-list [&_.check-row:first-child]:border-t-0">{checks.map((check) => <CheckRow key={check.id} check={check} />)}</div>
        </section>
      )}
    </section>
  );
}

function CheckRow({ check }: { check: VerificationCheck }): React.JSX.Element {
  const title = CHECK_TITLES[check.id] ?? check.title;
  return (
    <div className="row min-h-12.5 pt-2.25 pr-3 pb-2.25 pl-3 flex items-center gap-3 border-b border-b-border last:border-b-0 check-row [&_.row-note]:select-text min-h-9 border-t border-t-border border-b-0 [&_.row-main]:overflow-hidden [&_.row-main]:text-ellipsis grid grid-cols-[18px_minmax(0,_1fr)_auto] items-start gap-3 pt-3 pr-3.5 pb-3 pl-3.5 [&_.row-main]:min-w-0 [&_.row-main]:grid [&_.row-main]:whitespace-normal [&_.row-note]:mt-1.25">
      <span className={`check-icon w-4.5 h-4.5 flex-none grid place-items-center rounded-full [&.check-pass]:text-primary-foreground [&.check-pass]:bg-primary [&.check-fail]:text-destructive [&.check-fail]:bg-[var(--danger-bg)] [&.check-skip]:text-warning [&.check-skip]:bg-[var(--warning-bg)] [&.check-info]:text-warning [&.check-info]:bg-[var(--warning-bg)] check-${check.status}`} aria-hidden="true">
        {check.status === "pass" && <Check size={12} />}
      </span>
      <span className="row-main min-w-0 flex-auto flex flex-wrap items-center gap-y-0.5 gap-x-2"><span className="row-title font-medium">{title}</span><span className="row-note flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap">{check.detail}</span></span>
      <span className={`[&.result-pass]:text-primary [&.result-fail]:text-destructive [&.result-skip]:text-warning [&.result-info]:text-warning result flex-none text-xs font-semibold result-${check.status}`}>{checkStatusLabel(check.status)}</span>
    </div>
  );
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
