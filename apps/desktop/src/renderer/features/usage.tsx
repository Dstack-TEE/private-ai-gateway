import React, { lazy, Suspense, useEffect, useId, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { usagePageQuery } from "../lib/page-queries";
import { errorMessage } from "../lib/error-message";
import { Ban, Check, ChevronLeft, ChevronRight, Copy, ShieldCheck, ShieldX } from "lucide-react";
import { Button } from "../components/ui/button";
import { ActionItem } from "../components/action-item";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../components/ui/tabs";
import { UsageChart, type UsageMetric } from "../components/usage-chart";
import { StateLabel } from "../components/state-label";
import { Hint } from "../components/hint";
import { agentName, currency, formatTokens, outcomeOf, usageTokens } from "../lib/usage-presentation";
import { USAGE_PAGE_SIZES, USAGE_SEARCH_DEFAULTS, usageDateBounds, usageDateLabel, usageDateSearch, usageDateSelection, type UsageSearch } from "../lib/usage-dates";
import { Field, FieldLabel, FieldSet, FieldLegend } from "../components/ui/field";
import { Alert, AlertDescription, AlertTitle } from "../components/ui/alert";
import { Card, CardHeader, CardTitle, CardDescription, CardAction, CardContent } from "../components/ui/card";
import { IconButton } from "../components/controls";
import { AppDialog, DoneFooter } from "../components/app-dialog";
import { desktopApi } from "../lib/environment";
import { ChoiceSelect } from "../components/choice-select";
import type { RequestActivity, UsagePage } from "../../shared/contracts";
import { useShell } from "../lib/shell";
import { formatTimestamp } from "../lib/format";
import { VerificationVerdict } from "../components/verification-verdict";

const UsageDatePicker = lazy(() => import("../components/usage-date-picker").then((module) => ({ default: module.UsageDatePicker })));

const UsageTable = lazy(() => import("../components/usage-table").then((module) => ({ default: module.UsageTable })));

export function UsageRow({ activity, onOpen }: { activity: RequestActivity; onOpen(): void }): React.JSX.Element {
  const outcome = outcomeOf(activity);
  const tokens = usageTokens(activity);
  const timestamp = new Date(activity.at * 1_000);
  return (
    <ActionItem size="xs" className="usage-row min-h-15.5 gap-2.5 overflow-hidden [&_.row-main]:min-w-0 [&_.row-main]:flex-1 [&_.row-title]:text-sm [&_.state]:ml-0.5 [&_time]:min-w-17.5 [&_time]:grid [&_time]:text-right @max-[480px]:[&_.usage-cost]:hidden @max-[480px]:[&_.usage-amount]:w-13 max-[620px]:items-start max-[620px]:flex-wrap max-[620px]:[&_.row-main]:flex-[1_1_calc(100%_-_88px)] max-[620px]:[&_time]:order-4 max-[620px]:[&_time]:flex-[1_0_100%] max-[620px]:[&_time]:pl-11 max-[440px]:[&_.row-main]:basis-[calc(100%_-_74px)] max-[440px]:[&_time]:pl-0" onClick={onOpen} aria-label={`${agentName(activity.agent)}, ${outcome.label}, ${activity.model ?? activity.path}. View proof`}>
      <span className="row-main min-w-0 flex-auto flex flex-wrap items-center gap-y-0.5 gap-x-2">
        <span className="row-title font-medium">{agentName(activity.agent)}</span>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        <code className="flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap">{activity.model ?? activity.path}</code>
      </span>
      <span className="usage-amount w-18.5 flex-none grid justify-items-end tabular-nums [&_strong]:max-w-full [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_small]:text-muted-foreground [&_small]:text-xs max-[620px]:w-17 max-[620px]:[&:nth-of-type(3)]:hidden max-[440px]:w-15.5"><strong className="font-medium">{tokens === undefined ? "—" : formatTokens(tokens)}</strong><small>tokens</small></span>
      <span className="usage-amount w-18.5 flex-none grid justify-items-end tabular-nums [&_strong]:max-w-full [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_small]:text-muted-foreground [&_small]:text-xs max-[620px]:w-17 max-[620px]:[&:nth-of-type(3)]:hidden max-[440px]:w-15.5 usage-cost"><strong className="font-medium">{activity.costUsd === undefined ? "—" : currency(activity.costUsd)}</strong><small>cost</small></span>
      <time className="row-side flex-none text-muted-foreground text-xs tabular-nums whitespace-nowrap" dateTime={timestamp.toISOString()}><span>{timestamp.toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span><span>{formatTimestamp(timestamp.getTime())}</span></time>
    </ActionItem>
  );
}

export function UsagePage(): React.JSX.Element {
  const shell = useShell();
  const agents = shell.agents.agents;
  const onInspect = (activity: RequestActivity) => shell.openDialog({ kind: "usage-proof", activity });
  const search = useSearch({ from: "/_app/usage" });
  const navigate = useNavigate({ from: "/usage" });
  const filter = (next: UsageSearch) => void navigate({ search: (current) => ({ ...current, ...next }) });
  const agent = search.agent ?? "";
  const model = search.model ?? "";
  const range = usageDateSelection(search);
  const pageSize = search.rows ?? USAGE_SEARCH_DEFAULTS.rows;
  const [metric, setMetric] = useState<UsageMetric>("tokens");
  const bounds = usageDateBounds(range);
  const { since, until } = bounds;
  const filters = { agent: search.agent, model: search.model, since, until, limit: pageSize };
  // Cursors are opaque positions in one filtered result, so the page stack
  // stays in memory and restarts whenever the filters in the URL change.
  const filterKey = JSON.stringify(filters);
  const [pagination, setPagination] = useState({ filterKey, cursors: [undefined] as (string | undefined)[] });
  const cursors = pagination.filterKey === filterKey ? pagination.cursors : [undefined];
  const focusAfterPage = useRef(false);
  const usageQuery = { ...filters, cursor: cursors[cursors.length - 1] };
  const { data: page, error: queryError, isPending: loading } = useQuery(usagePageQuery(usageQuery));
  const error = queryError ? errorMessage(queryError) : undefined;
  useEffect(() => {
    if (!loading && focusAfterPage.current) {
      focusAfterPage.current = false;
      window.requestAnimationFrame(() => document.getElementById("usage-history-title")?.focus());
    }
  }, [loading, page, queryError]);

  const agentOptions = Array.from(new Set([
    ...(agent ? [agent] : []),
    ...agents.map((entry) => entry.id),
    ...(page?.agents ?? []),
  ]));
  const modelOptions = Array.from(new Set([...(model ? [model] : []), ...(page?.models ?? [])]));

  return (
    <div className="usage-page max-w-230 min-h-full mt-0 mr-auto mb-0 ml-auto">
      {error && <Alert variant="destructive" className="mb-4"><AlertTitle>Could not load usage</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}
      <div className="usage-toolbar grid grid-cols-[minmax(150px,_0.8fr)_minmax(210px,_1.25fr)_auto] items-end gap-2.5 [&_select]:w-full [&_select]:min-w-0 max-[780px]:grid-cols-2 max-[440px]:grid-cols-1" role="group" aria-label="Usage filters">
        <Field><FieldLabel htmlFor="usage-agent">Agent</FieldLabel><ChoiceSelect id="usage-agent" label="Agent" className="w-full" value={agent} onChange={(value) => filter({ agent: value || undefined })} options={[{ value: "", label: "All agents" }, ...agentOptions.map((entry) => ({ value: entry, label: agentName(entry) }))]} /></Field>
        <Field><FieldLabel htmlFor="usage-model">Model</FieldLabel><ChoiceSelect id="usage-model" label="Model" className="w-full" value={model} onChange={(value) => filter({ model: value || undefined })} options={[{ value: "", label: "All models" }, ...modelOptions.map((entry) => ({ value: entry, label: entry }))]} /></Field>
        <FieldSet className="time-filter max-[780px]:col-span-full max-[440px]:col-auto min-w-0 gap-0">
          <FieldLegend variant="label" className="leading-snug">Time</FieldLegend>
          <Suspense fallback={<Button variant="outline" disabled>{usageDateLabel(range)}</Button>}><UsageDatePicker value={range} onChange={(next) => filter(usageDateSearch(next))} /></Suspense>
        </FieldSet>
      </div>
      <UsageStats page={page} />
      <Card size="sm" role="region" className="usage-over-time mt-4" aria-labelledby="usage-chart-title">
        <Tabs value={metric} onValueChange={(value) => { if (value === "tokens" || value === "cost" || value === "requests") setMetric(value); }}>
          <CardHeader className="items-center gap-3 max-[440px]:grid-cols-1">
            <CardTitle><h2 id="usage-chart-title">Usage over time</h2></CardTitle>
            <CardAction className="max-[440px]:col-start-1 max-[440px]:row-start-2 max-[440px]:justify-self-start"><TabsList aria-label="Chart metric"><TabsTrigger value="tokens">Tokens</TabsTrigger><TabsTrigger value="cost">Cost</TabsTrigger><TabsTrigger value="requests">Requests</TabsTrigger></TabsList></CardAction>
          </CardHeader>
          <CardContent><TabsContent value={metric}><UsageChart page={page} loading={loading && !page} range={range.preset} bounds={bounds} metric={metric} /></TabsContent></CardContent>
        </Tabs>
      </Card>
      <Card size="sm" role="region" className="usage-history mt-4" aria-labelledby="usage-history-title">
        <CardHeader><CardTitle><h2 id="usage-history-title" tabIndex={-1}>Usage history</h2></CardTitle>
          <CardDescription aria-live="polite">{loading ? "Loading" : page ? `${page.summary.requests} records · kept on this device` : "Unavailable"}</CardDescription>
        </CardHeader>
        <CardContent><Suspense fallback={<div className="h-80" aria-busy="true" />}><UsageTable items={page?.items ?? []} loading={loading && !page} pageIndex={cursors.length - 1} pageSize={pageSize} total={page?.summary.requests ?? 0} onInspect={onInspect} /></Suspense>
        <div className="pagination mt-2.5 flex flex-wrap items-center justify-center gap-3 [&_>_span]:min-w-32 [&_>_span]:text-muted-foreground [&_>_span]:text-center">
          <Field orientation="horizontal" className="w-auto">
            <FieldLabel htmlFor="usage-page-size">Rows per page</FieldLabel>
            <ChoiceSelect id="usage-page-size" label="Rows per page" size="sm" value={String(pageSize)} disabled={loading} onChange={(value) => filter({ rows: USAGE_PAGE_SIZES.find((size) => String(size) === value) })} options={USAGE_PAGE_SIZES.map((size) => ({ value: String(size), label: String(size) }))} />
          </Field>
          <IconButton
            label="Previous usage page"
            disabled={loading || cursors.length === 1}
            onClick={() => {
              focusAfterPage.current = true;
              setPagination({ filterKey, cursors: cursors.slice(0, -1) });
            }}
          ><ChevronLeft size={16} /></IconButton>
          <span role="status" aria-live="polite">
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
              if (!next) return;
              focusAfterPage.current = true;
              setPagination({ filterKey, cursors: [...cursors, next] });
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
  return <div className="usage-stats mt-4 grid grid-cols-4 gap-4 max-[780px]:grid-cols-2">
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
    <dl className="evidence [&_dd]:select-text grid grid-cols-[82px_minmax(0,_1fr)] gap-y-3.5 gap-x-4 text-sm [&_dt]:text-muted-foreground [&_dt]:font-semibold [&_dd]:min-w-0 [&_dd]:text-muted-foreground [&_dd]:wrap-anywhere [&_dd_>_code]:block [&_dd_>_code]:mt-0.5 [&_dd_>_code]:text-muted-foreground max-[440px]:grid-cols-1 max-[440px]:[&_dt]:mt-1.25">
      <dt>Request</dt>
      <dd>
        {agentName(activity.agent)} <code>{activity.method} {activity.path}</code>
      </dd>
      {activity.model && <><dt>Model</dt><dd><code>{activity.model}</code></dd></>}
      <dt>Outcome</dt>
      <dd>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        {failed && <span className="dim text-muted-foreground"> HTTP {activity.status}</span>}
      </dd>
      <dt>Network</dt>
      <dd>
        {!activity.leftDevice
          ? "Blocked locally; request content did not leave this device."
          : deliveryUnconfirmed
            ? "The request entered upstream delivery; whether the service received it could not be confirmed."
            : "Forwarded to the attested service."}
      </dd>
      <dt>Usage</dt>
      <dd>
        <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 tabular-nums">
          <dt>Input tokens</dt><dd className="text-right">{activity.inputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</dd>
          <dt>Output tokens</dt><dd className="text-right">{activity.outputTokens?.toLocaleString() ?? <MissingUsage activity={activity} />}</dd>
          {activity.cacheReadTokens !== undefined && <><dt>Cache read</dt><dd className="text-right">{activity.cacheReadTokens.toLocaleString()}</dd></>}
          {activity.cacheWriteTokens !== undefined && <><dt>Cache write</dt><dd className="text-right">{activity.cacheWriteTokens.toLocaleString()}</dd></>}
          {activity.costUsd !== undefined && <><dt>Cost</dt><dd className="text-right">{currency(activity.costUsd)}</dd></>}
        </dl>
      </dd>
      {activity.receiptId && (
        <>
          <dt>Receipt ID</dt>
          <dd><code>{activity.receiptId}</code></dd>
        </>
      )}
      {notes.length > 0 && (
        <>
          <dt>Notes</dt>
          <dd>{notes.join(" ")}</dd>
        </>
      )}
    </dl>
    {activity.detail && <section className="proof-explanation pt-3.5 border-t border-t-border [&_h3]:text-xs [&_h3]:font-semibold [&_p]:mt-1.5 [&_p]:text-muted-foreground [&_p]:text-xs" aria-label="Verification details"><h3>Verification details</h3><p className="break-words whitespace-pre-wrap">{activity.detail}</p></section>}
    {activity.leftDevice && <section className="proof-explanation pt-3.5 border-t border-t-border [&_h3]:text-xs [&_h3]:font-semibold [&_p]:mt-1.5 [&_p]:text-muted-foreground [&_p]:text-xs" aria-label="Proof scope">
      <h3>What the proof checks</h3>
      <p>The verifier checks the request digest, the service signature against its attested keyset, and the response digest. This verifies the exchanged data, not answer accuracy.</p>
    </section>}
    </>
  );
}

/** Refreshes the record on open: its receipt may have been verified since the list loaded. */
export function UsageProofDialog({ activity: listed, onClose }: { activity: RequestActivity; onClose(): void }): React.JSX.Element {
  const { data: activity = listed, error } = useQuery({
    queryKey: ["usage-record", listed.id], queryFn: () => desktopApi.getUsageRecord(listed.id), initialData: listed,
  });
  return (
    <AppDialog title="Usage proof" description={formatTimestamp(activity.at * 1_000, true)} className="sm:max-w-xl" onClose={onClose}>
      {error && <Alert variant="destructive"><AlertTitle>Could not refresh this record</AlertTitle><AlertDescription>{errorMessage(error)}</AlertDescription></Alert>}
      <div className="-mx-6 flex min-h-0 flex-col gap-5 overflow-y-auto px-6 [&_.evidence]:text-sm [&_.evidence]:gap-y-4.5 [&_[data-slot=verification-verdict]]:shrink-0"><Evidence activity={activity} />{activity.receiptId && <SignedReceipt recordId={activity.id} />}</div>
      <DoneFooter />
    </AppDialog>
  );
}

/**
 * The receipt document the audit checked. The signature covers the document's
 * JCS form, whose numbers and strings serialize as ECMAScript's do (RFC 8785),
 * so indenting it with `JSON.stringify` shows exactly the signed values. Copy
 * takes the document as the service returned it, ready for `pap audit --receipt`.
 */
function SignedReceipt({ recordId }: { recordId: string }): React.JSX.Element | null {
  const shell = useShell();
  const titleId = useId();
  const { data: receipt, error } = useQuery({
    queryKey: ["usage-receipt", recordId], queryFn: () => desktopApi.getUsageReceipt(recordId),
  });
  const formatted = useMemo(() => {
    if (!receipt) return undefined;
    try { return JSON.stringify(JSON.parse(receipt), null, 2); }
    catch { return receipt; }
  }, [receipt]);
  if (error) return <Alert variant="destructive"><AlertTitle>Could not load the signed receipt</AlertTitle><AlertDescription>{errorMessage(error)}</AlertDescription></Alert>;
  if (!receipt) return null;
  return <section className="grid gap-2 border-t border-t-border pt-3.5" aria-labelledby={titleId}>
    <div className="flex items-center justify-between gap-3">
      <div className="grid gap-0.5"><h3 id={titleId} className="text-xs font-semibold">Signed receipt</h3><p className="text-xs text-muted-foreground">As returned by the service. It holds hashes and verification metadata, not request or response content.</p></div>
      <IconButton size="icon-sm" label="Copy signed receipt" onClick={() => shell.copy("Signed receipt", receipt)}>{shell.copied === "Signed receipt" ? <Check /> : <Copy />}</IconButton>
    </div>
    <pre className="overflow-x-auto rounded-2xl border bg-muted/50 p-4 text-xs leading-relaxed select-text"><code>{formatted}</code></pre>
  </section>;
}

function MissingUsage({ activity }: { activity: Pick<RequestActivity, "leftDevice" | "path"> }) {
  const notApplicable = !activity.leftDevice || activity.path === "/v1/messages/count_tokens";
  const explanation = !activity.leftDevice
    ? "This request was blocked locally before forwarding. There is no provider token usage to report."
    : activity.path === "/v1/messages/count_tokens"
      ? "This endpoint counts a prompt’s tokens; it does not return an inference usage report."
      : "No token count was recorded. The provider may omit usage, or the response may be incomplete or too large to capture. Missing counts are not estimated.";
  return <Hint content={explanation}><span tabIndex={0} className="text-muted-foreground underline decoration-dotted underline-offset-4">{notApplicable ? "Not applicable" : "Unavailable"}</span></Hint>;
}
