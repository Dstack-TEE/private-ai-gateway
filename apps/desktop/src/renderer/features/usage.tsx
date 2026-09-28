import React, { lazy, Suspense, useId, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { usageFilters, usagePageQuery } from "../lib/page-queries";
import { errorMessage } from "../lib/error-message";
import { useCopy } from "../hooks/use-copy";
import { Ban, Check, ChevronLeft, ChevronRight, Copy, ShieldCheck, ShieldX } from "lucide-react";
import { Button } from "../components/ui/button";
import { ActionItem } from "../components/action-item";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../components/ui/tabs";
import { UsageChart, type UsageMetric } from "../components/usage-chart";
import { StateLabel } from "../components/state-label";
import { HoverDetails } from "../components/hint";
import { agentName, currency, formatTokens, outcomeOf, usageTokens } from "../lib/usage-presentation";
import { USAGE_PAGE_SIZES, USAGE_SEARCH_DEFAULTS, usageDateBounds, usageDateLabel, usageDateSearch, usageDateSelection, type UsageSearch } from "../lib/usage-dates";
import { Field, FieldError, FieldLabel, FieldSet, FieldLegend } from "../components/ui/field";
import { Alert, AlertDescription, AlertTitle } from "../components/ui/alert";
import { Card, CardHeader, CardTitle, CardDescription, CardAction, CardContent } from "../components/ui/card";
import { IconButton } from "../components/controls";
import { AppDialog, AppDialogBody, DoneFooter, type DialogControl } from "../components/app-dialog";
import { desktopApi } from "../lib/environment";
import { ChoiceSelect } from "../components/choice-select";
import type { RequestActivity, UsagePage } from "../../shared/contracts";
import { useShell } from "../lib/shell";
import { formatTimestamp } from "../lib/format";
import { VerificationVerdict } from "../components/verification-verdict";
import { cn } from "../lib/utils";

const UsageDatePicker = lazy(() => import("../components/usage-date-picker").then((module) => ({ default: module.UsageDatePicker })));

const UsageTable = lazy(() => import("../components/usage-table").then((module) => ({ default: module.UsageTable })));

export function UsageRow({ activity, onOpen }: { activity: RequestActivity; onOpen(): void }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const tokens = usageTokens(activity);
  const timestamp = new Date(activity.at * 1_000);
  const actionId = useId();
  // The visible text names the row; the hidden commas pause between its parts.
  const pause = <span className="sr-only">, </span>;
  return (
    <ActionItem size="xs" className="min-h-15.5 gap-2.5 overflow-hidden max-[620px]:items-start" aria-haspopup="dialog" aria-describedby={actionId} onClick={onOpen}>
      <span className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-0.5 max-[620px]:flex-[1_1_calc(100%-88px)] max-[440px]:basis-[calc(100%-74px)]">
        <span className="font-medium">{agentName(activity.agent)}{pause}</span>
        <StateLabel tone={outcome.tone} text={outcome.label} />{pause}
        <code className="block flex-[1_0_100%] truncate text-xs wrap-anywhere text-muted-foreground">{activity.model ?? activity.path}{pause}</code>
      </span>
      <UsageAmount value={tokens === undefined ? "—" : formatTokens(tokens)} unit="tokens" pause={pause} />
      <UsageAmount value={activity.costUsd === undefined ? "—" : currency(activity.costUsd)} unit="cost" pause={pause} className="max-[620px]:hidden @max-[480px]:hidden" />
      <time className="grid min-w-17.5 flex-none text-right text-xs whitespace-nowrap text-muted-foreground tabular-nums max-[620px]:order-4 max-[620px]:flex-[1_0_100%] max-[620px]:pl-11 max-[440px]:pl-0" dateTime={timestamp.toISOString()}><span>{timestamp.toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span><span>{formatTimestamp(timestamp.getTime())}</span></time>
      <span id={actionId} hidden>View proof</span>
    </ActionItem>
  );
}

/** A figure over its unit, at the end of a usage row. */
function UsageAmount({ value, unit, pause, className }: { value: string; unit: string; pause: React.ReactNode; className?: string }): React.JSX.Element {
  return <span className={cn("grid w-18.5 flex-none justify-items-end tabular-nums max-[620px]:w-17 max-[440px]:w-15.5 @max-[480px]:w-13", className)}>
    <strong className="max-w-full truncate font-medium">{value}</strong><small className="text-xs text-muted-foreground">{unit}{pause}</small>
  </span>;
}

export function UsagePage(): React.JSX.Element {
  const shell = useShell();
  const agents = shell.agents.agents;
  const onInspect = (activity: RequestActivity) => shell.openDialog({ kind: "usage-proof", activity });
  const search = useSearch({ from: "/_app/usage" });
  const navigate = useNavigate({ from: "/usage" });
  // Filtering keeps the page where it is scrolled.
  const filter = (next: UsageSearch) => void navigate({ search: (current) => ({ ...current, ...next }), resetScroll: false });
  const agent = search.agent ?? "";
  const model = search.model ?? "";
  const range = usageDateSelection(search);
  const pageSize = search.rows ?? USAGE_SEARCH_DEFAULTS.rows;
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const bounds = usageDateBounds(range);
  const filters = usageFilters(search);
  // Cursors are opaque positions in one filtered result, so the page stack
  // stays in memory and restarts whenever the filters in the URL change.
  const filterKey = JSON.stringify(filters);
  const [pagination, setPagination] = useState<{ filterKey: string; cursors: (string | undefined)[] }>({ filterKey, cursors: [undefined] });
  const cursors = pagination.filterKey === filterKey ? pagination.cursors : [undefined];
  // Paging disables its buttons while it loads, so focus moves to the list.
  const historyTitle = useRef<HTMLHeadingElement>(null);
  const showPage = (next: (string | undefined)[]) => {
    historyTitle.current?.focus();
    setPagination({ filterKey, cursors: next });
  };
  const usageQuery = { ...filters, cursor: cursors[cursors.length - 1] };
  const { data: page, isPending: loading } = useQuery(usagePageQuery(usageQuery));

  const agentOptions = Array.from(new Set([
    ...(agent ? [agent] : []),
    ...agents.map((entry) => entry.id),
    ...(page?.agents ?? []),
  ]));
  const modelOptions = Array.from(new Set([...(model ? [model] : []), ...(page?.models ?? [])]));

  return (
    <div className="mx-auto min-h-full max-w-230">
      <div className="grid grid-cols-[minmax(150px,0.8fr)_minmax(210px,1.25fr)_auto] items-end gap-2.5 max-[780px]:grid-cols-2 max-[440px]:grid-cols-1" role="group" aria-label="Usage filters">
        <Field><FieldLabel htmlFor="usage-agent">Agent</FieldLabel><ChoiceSelect id="usage-agent" label="Agent" className="w-full" value={agent} onChange={(value) => filter({ agent: value || undefined })} options={[{ value: "", label: "All agents" }, ...agentOptions.map((entry) => ({ value: entry, label: agentName(entry) }))]} /></Field>
        <Field><FieldLabel htmlFor="usage-model">Model</FieldLabel><ChoiceSelect id="usage-model" label="Model" className="w-full" value={model} onChange={(value) => filter({ model: value || undefined })} options={[{ value: "", label: "All models" }, ...modelOptions.map((entry) => ({ value: entry, label: entry }))]} /></Field>
        <FieldSet className="max-[780px]:col-span-full max-[440px]:col-auto min-w-0 gap-0">
          <FieldLegend variant="label" className="leading-snug">Time</FieldLegend>
          <Suspense fallback={<Button variant="outline" disabled>{usageDateLabel(range)}</Button>}><UsageDatePicker value={range} onChange={(next) => filter(usageDateSearch(next))} /></Suspense>
        </FieldSet>
      </div>
      <UsageStats page={page} />
      <Card size="sm" role="region" className="mt-4" aria-labelledby="usage-chart-title">
        <Tabs value={metric} onValueChange={(value) => { if (value === "tokens" || value === "cost" || value === "requests") setMetric(value); }}>
          <CardHeader className="items-center gap-3 max-[440px]:grid-cols-1">
            <CardTitle><h2 id="usage-chart-title">Usage over time</h2></CardTitle>
            <CardAction className="max-[440px]:col-start-1 max-[440px]:row-start-2 max-[440px]:justify-self-start"><TabsList aria-label="Chart metric"><TabsTrigger value="tokens">Tokens</TabsTrigger><TabsTrigger value="cost">Cost</TabsTrigger><TabsTrigger value="requests">Requests</TabsTrigger></TabsList></CardAction>
          </CardHeader>
          <CardContent><TabsContent value={metric}><UsageChart page={page} loading={loading && !page} range={range.preset} bounds={bounds} metric={metric} /></TabsContent></CardContent>
        </Tabs>
      </Card>
      <Card size="sm" role="region" className="mt-4" aria-labelledby="usage-history-title">
        <CardHeader><CardTitle><h2 ref={historyTitle} id="usage-history-title" tabIndex={-1}>Usage history</h2></CardTitle>
          <CardDescription aria-live="polite">{loading ? "Loading" : page ? `${page.summary.requests} records · kept on this device` : "Unavailable"}</CardDescription>
        </CardHeader>
        <CardContent><Suspense fallback={<div className="h-80" aria-busy="true" />}><UsageTable items={page?.items ?? []} loading={loading && !page} pageIndex={cursors.length - 1} pageSize={pageSize} total={page?.summary.requests ?? 0} onInspect={onInspect} /></Suspense>
        <div className="mt-2.5 flex flex-wrap items-center justify-center gap-3">
          <Field orientation="horizontal" className="w-auto">
            <FieldLabel htmlFor="usage-page-size">Rows per page</FieldLabel>
            <ChoiceSelect id="usage-page-size" label="Rows per page" size="sm" value={String(pageSize)} disabled={loading} onChange={(value) => filter({ rows: USAGE_PAGE_SIZES.find((size) => String(size) === value) })} options={USAGE_PAGE_SIZES.map((size) => ({ value: String(size), label: String(size) }))} />
          </Field>
          <IconButton
            label="Previous usage page"
            disabled={loading || cursors.length === 1}
            onClick={() => showPage(cursors.slice(0, -1))}
          ><ChevronLeft size={16} /></IconButton>
          <span role="status" aria-live="polite" className="min-w-32 text-center text-muted-foreground">
            Page {cursors.length}
            {page && page.items.length > 0
              ? ` · ${(cursors.length - 1) * pageSize + 1}-${(cursors.length - 1) * pageSize + page.items.length} of ${page.summary.requests}`
              : ""}
          </span>
          <IconButton
            label="Next usage page"
            disabled={loading || !page?.nextCursor}
            onClick={() => {
              const next = page?.nextCursor;
              if (next) showPage([...cursors, next]);
            }}
          ><ChevronRight size={16} /></IconButton>
        </div>
      </CardContent></Card>
    </div>
  );
}

function UsageStats({ page }: { page?: UsagePage }): React.JSX.Element {
  const summary = page?.summary;
  const totalTokens = (summary?.inputTokens ?? 0) + (summary?.outputTokens ?? 0);
  const forwarded = Math.max(0, (summary?.requests ?? 0) - (summary?.blockedLocally ?? 0));
  const protectedRate = forwarded ? (summary?.protected ?? 0) / forwarded : 0;
  const failedOrRejected = (summary?.blockedLocally ?? 0) + (summary?.failedProof ?? 0);
  const stats = [
    ["Requests", summary ? summary.requests.toLocaleString() : "—", summary ? `${failedOrRejected.toLocaleString()} failed or rejected` : "—"],
    ["Tokens", summary ? formatTokens(totalTokens) : "—", summary ? `${formatTokens(summary.inputTokens)} in · ${formatTokens(summary.outputTokens)} out` : "—"],
    ["Estimated cost", summary ? currency(summary.costUsd) : "—", "Based on model prices"],
    ["Protected", forwarded ? `${Math.round(protectedRate * 100)}%` : "—", summary ? `${summary.protected} of ${forwarded} responses` : "—"],
  ];
  return <div className="mt-4 grid grid-cols-4 gap-4 max-[780px]:grid-cols-2">
    {stats.map(([label, value, detail]) => <Card key={label} size="sm" className="min-w-0"><CardContent className="grid gap-1"><span className="text-xs text-muted-foreground">{label}</span><strong className="truncate text-xl font-semibold tabular-nums">{value}</strong><small className="truncate text-xs text-muted-foreground">{detail}</small></CardContent></Card>)}
  </div>;
}

function Evidence({ activity }: { activity: RequestActivity }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const receiptVerified = activity.leftDevice && activity.verified === true && Boolean(activity.receiptId);
  const ReceiptIcon = !activity.leftDevice ? Ban : receiptVerified ? ShieldCheck : ShieldX;
  const failed = activity.leftDevice && (activity.status < 200 || activity.status >= 300);
  const deliveryUnconfirmed = activity.leftDevice
    && activity.verified !== false
    && !activity.receiptId
    && (activity.status === 502 || activity.status === 504);
  const notes = [
    activity.streamed ? "Streamed response." : undefined,
    activity.localPolicyApplied
      ? "The verifier applied its routing policy before sending; the receipt binds those bytes."
      : undefined,
    activity.rewritten ? "The service rewrote the request before inference; the receipt records it." : undefined,
  ].filter(Boolean);
  const verdictTone = receiptVerified ? "success" : activity.leftDevice && activity.verified === false ? "danger" : "neutral";
  return (
    <>
    <VerificationVerdict
      tone={verdictTone}
      icon={ReceiptIcon}
      title={!activity.leftDevice ? "Request kept on this device" : receiptVerified ? "Signed receipt verified" : activity.verified === false ? "Receipt verification failed" : "No verified receipt"}
      detail={!activity.leftDevice ? "Nothing was sent to the provider. No remote receipt is needed." : activity.verified === false ? "Receipt audit failed. Content may already have reached the client and cannot be retracted. See the recorded reason below." : receiptVerified ? "The signed receipt matches the request and response bytes recorded by the verifier." : "No successful verification result is recorded for this request."}
    />
    <dl className="grid grid-cols-[82px_minmax(0,1fr)] gap-x-4 gap-y-4.5 text-sm max-[440px]:grid-cols-1">
      <EvidenceTerm>Request</EvidenceTerm>
      <EvidenceValue>
        {agentName(activity.agent)} <EvidenceCode>{activity.method} {activity.path}</EvidenceCode>
      </EvidenceValue>
      {activity.model && <><EvidenceTerm>Model</EvidenceTerm><EvidenceValue><EvidenceCode>{activity.model}</EvidenceCode></EvidenceValue></>}
      <EvidenceTerm>Outcome</EvidenceTerm>
      <EvidenceValue>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        {failed && <span className="text-muted-foreground"> HTTP {activity.status}</span>}
      </EvidenceValue>
      <EvidenceTerm>Network</EvidenceTerm>
      <EvidenceValue>
        {!activity.leftDevice
          ? "Blocked locally; request content did not leave this device."
          : deliveryUnconfirmed
            ? "The request entered upstream delivery; whether the service received it could not be confirmed."
            : "Forwarded to the attested service."}
      </EvidenceValue>
      <EvidenceTerm>Usage</EvidenceTerm>
      <EvidenceValue>
        <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 tabular-nums">
          <EvidenceTerm>Input tokens</EvidenceTerm><EvidenceValue className="text-right">{activity.inputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</EvidenceValue>
          <EvidenceTerm>Output tokens</EvidenceTerm><EvidenceValue className="text-right">{activity.outputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</EvidenceValue>
          {activity.cacheReadTokens !== undefined && <><EvidenceTerm>Cache read</EvidenceTerm><EvidenceValue className="text-right">{activity.cacheReadTokens.toLocaleString()}</EvidenceValue></>}
          {activity.cacheWriteTokens !== undefined && <><EvidenceTerm>Cache write</EvidenceTerm><EvidenceValue className="text-right">{activity.cacheWriteTokens.toLocaleString()}</EvidenceValue></>}
          {activity.costUsd !== undefined && <><EvidenceTerm>Cost</EvidenceTerm><EvidenceValue className="text-right">{currency(activity.costUsd)}</EvidenceValue></>}
        </dl>
      </EvidenceValue>
      {activity.receiptId && (
        <>
          <EvidenceTerm>Receipt ID</EvidenceTerm>
          <EvidenceValue><EvidenceCode>{activity.receiptId}</EvidenceCode></EvidenceValue>
        </>
      )}
      {notes.length > 0 && (
        <>
          <EvidenceTerm>Notes</EvidenceTerm>
          <EvidenceValue>{notes.join(" ")}</EvidenceValue>
        </>
      )}
    </dl>
    {activity.detail && <ProofNote label="Verification details" title="Verification details" className="break-words whitespace-pre-wrap">{activity.detail}</ProofNote>}
    {activity.leftDevice && <ProofNote label="Proof scope" title="What the proof checks">
      The verifier checks the request digest, the service signature against its attested keyset, and the response digest. This verifies the exchanged data, not answer accuracy.
    </ProofNote>}
    </>
  );
}

function EvidenceTerm({ children }: React.PropsWithChildren): React.JSX.Element {
  return <dt className="font-semibold text-muted-foreground max-[440px]:mt-1.25">{children}</dt>;
}

function EvidenceValue({ className, children }: React.PropsWithChildren<{ className?: string }>): React.JSX.Element {
  return <dd className={cn("min-w-0 select-text wrap-anywhere text-muted-foreground", className)}>{children}</dd>;
}

function EvidenceCode({ children }: React.PropsWithChildren): React.JSX.Element {
  return <code className="mt-0.5 block text-muted-foreground">{children}</code>;
}

function ProofNote({ label, title, className, children }: React.PropsWithChildren<{ label: string; title: string; className?: string }>): React.JSX.Element {
  return <section className="border-t pt-3.5" aria-label={label}>
    <h3 className="text-xs font-semibold">{title}</h3>
    <p className={cn("mt-1.5 text-xs text-muted-foreground", className)}>{children}</p>
  </section>;
}

/** Refreshes the record on open: its receipt may have been verified since the list loaded. */
export function UsageProofDialog({ activity: listed, ...control }: { activity: RequestActivity } & DialogControl): React.JSX.Element {
  const { data: activity = listed, error } = useQuery({
    queryKey: ["usage-record", listed.id], queryFn: () => desktopApi.getUsageRecord(listed.id), initialData: listed, initialDataUpdatedAt: 0,
  });
  return (
    <AppDialog {...control} title="Usage proof" description={formatTimestamp(activity.at * 1_000, true)} className="sm:max-w-xl">
      {error && <Alert variant="destructive"><AlertTitle>Could not refresh this record</AlertTitle><AlertDescription>{errorMessage(error)}</AlertDescription></Alert>}
      <AppDialogBody className="flex flex-col gap-5"><Evidence activity={activity} />{activity.receiptId && <SignedReceipt recordId={activity.id} />}</AppDialogBody>
      <DoneFooter />
    </AppDialog>
  );
}

/**
 * A receipt document indented for reading. Parsing keeps every string and
 * safe integer exactly, so only whitespace changes; a document with any other
 * number (the verifier accepts any 64-bit integer), or one that is not valid
 * JSON, is shown as returned.
 */
function readableReceipt(receipt: string): string {
  let exact = true;
  let document: unknown;
  try {
    document = JSON.parse(receipt, (_key, value: unknown) => {
      if (typeof value === "number" && !Number.isSafeInteger(value)) exact = false;
      return value;
    });
  } catch {
    return receipt;
  }
  return exact ? JSON.stringify(document, null, 2) : receipt;
}

/** The receipt document the audit checked. Copy takes it as the service returned it, ready for `pap audit --receipt`. */
function SignedReceipt({ recordId }: { recordId: string }): React.JSX.Element | null {
  const { copy, isCopied, status, error: copyError } = useCopy();
  const titleId = useId();
  const { data: receipt, error } = useQuery({
    queryKey: ["usage-receipt", recordId], queryFn: () => desktopApi.getUsageReceipt(recordId),
  });
  const readable = useMemo(() => receipt && readableReceipt(receipt), [receipt]);
  if (error) return <Alert variant="destructive"><AlertTitle>Could not load the signed receipt</AlertTitle><AlertDescription>{errorMessage(error)}</AlertDescription></Alert>;
  if (!receipt) return null;
  return <section className="grid gap-2 border-t border-t-border pt-3.5">
    <div className="flex items-center justify-between gap-3">
      <div className="grid gap-0.5"><h3 id={titleId} className="text-xs font-semibold">Signed receipt</h3><p className="text-xs text-muted-foreground">Indented for reading; Copy gives the original document. It holds hashes and verification metadata, not request or response content.</p></div>
      <IconButton size="icon-sm" label="Copy signed receipt" onClick={() => copy("Signed receipt", receipt)}>{isCopied(receipt) ? <Check /> : <Copy />}</IconButton>
    </div>
    {status}
    {copyError && <FieldError>Could not copy the signed receipt. {errorMessage(copyError)}</FieldError>}
    <pre role="region" tabIndex={0} aria-labelledby={titleId} className="overflow-x-auto rounded-2xl border bg-muted/50 p-4 text-xs leading-relaxed select-text"><code>{readable}</code></pre>
  </section>;
}

function MissingUsage({ activity }: { activity: Pick<RequestActivity, "leftDevice" | "path"> }) {
  const notApplicable = !activity.leftDevice || activity.path === "/v1/messages/count_tokens";
  const explanation = !activity.leftDevice
    ? "This request was blocked locally before forwarding. There is no provider token usage to report."
    : activity.path === "/v1/messages/count_tokens"
      ? "This endpoint counts a prompt’s tokens; it does not return an inference usage report."
      : "No token count was recorded. The provider may omit usage, or the response may be incomplete or too large to capture. Missing counts are not estimated.";
  return <HoverDetails value={<span className="text-muted-foreground">{notApplicable ? "Not applicable" : "Unavailable"}</span>}>{explanation}</HoverDetails>;
}
