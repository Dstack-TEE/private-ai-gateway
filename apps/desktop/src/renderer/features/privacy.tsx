import React from "react";
import { Check, Info, LockOpen, Minus, RefreshCw, ShieldCheck, ShieldX, X, type LucideIcon } from "lucide-react";
import { AppDialog, DoneFooter, type DialogControl } from "../components/app-dialog";
import type { AppState, VerificationCheck } from "../../shared/contracts";
import { hasLiveVerification } from "../lib/protection";
import { formatTimestamp, hardwareName, shorten, trustName } from "../lib/format";
import { Detail } from "../components/detail";
import { StateLabel } from "../components/state-label";
import { Alert, AlertDescription, AlertTitle } from "../components/ui/alert";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "../components/ui/item";
import type { Tone } from "../../shared/contracts";

const CHECK_PRESENTATION: Record<VerificationCheck["status"], { icon: LucideIcon; tone: Tone }> = {
  pass: { icon: Check, tone: "success" },
  fail: { icon: X, tone: "danger" },
  skip: { icon: Minus, tone: "warning" },
  info: { icon: Info, tone: "warning" },
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

export function PrivacyDialog({ state, ...control }: { state: AppState } & DialogControl): React.JSX.Element {
  return <AppDialog {...control} title="Privacy verification" className="sm:max-w-2xl">
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
  const failed = !verified && (state.status === "blocked" || state.status === "error");
  const VerdictIcon = verified ? ShieldCheck : state.status === "verifying" ? RefreshCw : ShieldX;
  return (
    <section aria-label="Privacy">
      <Alert role="status" variant={failed ? "destructive" : "default"}>
        <VerdictIcon aria-hidden="true" />
        <AlertTitle>{verified ? "Service identity and connection verified" : state.status === "verifying" ? "Checking the service" : "No verified live connection"}</AlertTitle>
        <AlertDescription>{verified ? "This app checked the service’s hardware evidence and bound the encrypted connection to its attested key." : "A saved profile is not evidence of a currently protected connection. Protection must establish a new verified session."}</AlertDescription>
      </Alert>
      <ItemGroup className="mt-4">
        {facts.map((fact) => (
          <Item key={fact.title} variant="outline" size="sm">
            <ItemMedia variant="icon">{fact.ok ? <Check aria-hidden="true" /> : <LockOpen aria-hidden="true" />}</ItemMedia>
            <ItemContent className="min-w-0">
              <ItemTitle>{fact.title}</ItemTitle>
              <ItemDescription className="line-clamp-none wrap-anywhere">{fact.detail}</ItemDescription>
            </ItemContent>
          </Item>
        ))}
      </ItemGroup>
      <p className="mt-3 mb-4.5 text-xs text-muted-foreground">{passed("id-5") ? "Key custody evidence passed." : "Key custody is not independently established by these checks."} Responses are forwarded immediately; receipts are audited afterward and cannot retract delivered content. This summary does not verify upstream inference or answer accuracy. {!state.config.requireProductionOs && "Development OS images are allowed."}</p>
      {identity && (
        <section className="mt-6" aria-labelledby="verified-identity-title">
          <SectionHeading id="verified-identity-title" title={verified ? "Current service identity" : "Last reported identity"} summary={`${checkCount(checks)} checks passed`} />
          <div className="grid grid-cols-2 gap-x-5 gap-y-4 p-3.5">
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
        <section className="mt-6" aria-labelledby="verification-checks-title">
          <SectionHeading id="verification-checks-title" title="Verification checks" summary={`${checks.length} total`} />
          <ItemGroup>{checks.map((check) => <CheckRow key={check.id} check={check} />)}</ItemGroup>
        </section>
      )}
    </section>
  );
}

function CheckRow({ check }: { check: VerificationCheck }): React.JSX.Element {
  const title = CHECK_TITLES[check.id] ?? check.title;
  const { icon: Icon, tone } = CHECK_PRESENTATION[check.status];
  return (
    <Item variant="outline" size="sm">
      <ItemMedia variant="icon"><Icon aria-hidden="true" /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>{title}</ItemTitle>
        <ItemDescription className="line-clamp-none select-text wrap-anywhere">{check.detail}</ItemDescription>
      </ItemContent>
      <ItemActions><StateLabel tone={tone} text={checkStatusLabel(check.status)} /></ItemActions>
    </Item>
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
