import React, { lazy, Suspense, useId, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { usageFilters, usagePageQuery } from "../lib/page-queries";
import { errorMessage } from "../lib/error-message";
import { useCopy } from "../hooks/use-copy";
import { Ban, Check, ChevronLeft, ChevronRight, Copy, ShieldCheck, ShieldX } from "lucide-react";
import { Button } from "../components/ui/button";
import { Item, ItemContent, ItemDescription, ItemTitle } from "../components/ui/item";
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
import { AppDialog, DoneFooter, type DialogControl } from "../components/app-dialog";
import { desktopApi } from "../lib/environment";
import { ChoiceSelect } from "../components/choice-select";
import type { RequestActivity, UsagePage } from "../../shared/contracts";
import { useShell } from "../lib/shell";
import { formatTimestamp } from "../lib/format";
import { CodeBlock } from "../components/code-block";

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
    <>
      <Item variant="muted" size="xs" render={<button type="button" aria-haspopup="dialog" aria-describedby={actionId} onClick={onOpen} />}>
        <ItemContent className="min-w-0">
          <ItemTitle>{agentName(activity.agent)}{pause}<StateLabel tone={outcome.tone} text={outcome.label} />{pause}</ItemTitle>
          <ItemDescription className="truncate"><code>{activity.model ?? activity.path}</code>{pause}</ItemDescription>
        </ItemContent>
        <ItemContent className="items-end">
          <ItemTitle>{tokens === undefined ? "—" : formatTokens(tokens)}</ItemTitle>
          <ItemDescription>tokens{pause}</ItemDescription>
        </ItemContent>
        <ItemContent className="items-end @max-[480px]:hidden">
          <ItemTitle>{activity.costUsd === undefined ? "—" : currency(activity.costUsd)}</ItemTitle>
          <ItemDescription>cost{pause}</ItemDescription>
        </ItemContent>
        <ItemContent className="items-end">
          <ItemTitle><time dateTime={timestamp.toISOString()}>{formatTimestamp(timestamp.getTime())}</time></ItemTitle>
          <ItemDescription>{timestamp.toLocaleDateString(undefined, { month: "short", day: "numeric" })}</ItemDescription>
        </ItemContent>
      </Item>
      <span id={actionId} hidden>View proof</span>
    </>
  );
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
  const { data: page, error: queryError, isPending: loading } = useQuery(usagePageQuery(usageQuery));
  const error = queryError ? errorMessage(queryError) : undefined;

  const agentOptions = Array.from(new Set([
    ...(agent ? [agent] : []),
    ...agents.map((entry) => entry.id),
    ...(page?.agents ?? []),
  ]));
  const modelOptions = Array.from(new Set([...(model ? [model] : []), ...(page?.models ?? [])]));

  return (
    <div className="mx-auto flex max-w-230 flex-col gap-4">
      {error && <Alert variant="destructive"><AlertTitle>Could not load usage</AlertTitle><AlertDescription>{error}</AlertDescription></Alert>}
      <div className="grid grid-cols-[minmax(150px,0.8fr)_minmax(210px,1.25fr)_auto] items-end gap-2.5 max-[780px]:grid-cols-2 max-[440px]:grid-cols-1" role="group" aria-label="Usage filters">
        <Field><FieldLabel htmlFor="usage-agent">Agent</FieldLabel><ChoiceSelect id="usage-agent" label="Agent" className="w-full" value={agent} onChange={(value) => filter({ agent: value || undefined })} options={[{ value: "", label: "All agents" }, ...agentOptions.map((entry) => ({ value: entry, label: agentName(entry) }))]} /></Field>
        <Field><FieldLabel htmlFor="usage-model">Model</FieldLabel><ChoiceSelect id="usage-model" label="Model" className="w-full" value={model} onChange={(value) => filter({ model: value || undefined })} options={[{ value: "", label: "All models" }, ...modelOptions.map((entry) => ({ value: entry, label: entry }))]} /></Field>
        <FieldSet className="min-w-0 gap-0 max-[780px]:col-span-full max-[440px]:col-auto">
          <FieldLegend variant="label">Time</FieldLegend>
          <Suspense fallback={<Button variant="outline" disabled>{usageDateLabel(range)}</Button>}><UsageDatePicker value={range} onChange={(next) => filter(usageDateSearch(next))} /></Suspense>
        </FieldSet>
      </div>
      <UsageStats page={page} />
      <Card size="sm" role="region" aria-labelledby="usage-chart-title">
        <Tabs value={metric} onValueChange={(value) => { if (value === "tokens" || value === "cost" || value === "requests") setMetric(value); }}>
          <CardHeader className="items-center gap-3 max-[440px]:grid-cols-1">
            <CardTitle><h2 id="usage-chart-title">Usage over time</h2></CardTitle>
            <CardAction className="max-[440px]:col-start-1 max-[440px]:row-start-2 max-[440px]:justify-self-start"><TabsList aria-label="Chart metric"><TabsTrigger value="tokens">Tokens</TabsTrigger><TabsTrigger value="cost">Cost</TabsTrigger><TabsTrigger value="requests">Requests</TabsTrigger></TabsList></CardAction>
          </CardHeader>
          <CardContent><TabsContent value={metric}><UsageChart page={page} loading={loading && !page} range={range.preset} bounds={bounds} metric={metric} /></TabsContent></CardContent>
        </Tabs>
      </Card>
      <Card size="sm" role="region" aria-labelledby="usage-history-title">
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
          ><ChevronLeft /></IconButton>
          <span className="min-w-32 text-center text-muted-foreground" role="status" aria-live="polite">
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
          ><ChevronRight /></IconButton>
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
  return <div className="grid grid-cols-4 gap-4 max-[780px]:grid-cols-2">
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
  const receiptFailed = activity.leftDevice && activity.verified === false;
  return (
    <>
    <Alert role="status" variant={receiptFailed ? "destructive" : "default"}>
      <ReceiptIcon aria-hidden="true" />
      <AlertTitle>{!activity.leftDevice ? "Request kept on this device" : receiptVerified ? "Signed receipt verified" : activity.verified === false ? "Receipt verification failed" : "No verified receipt"}</AlertTitle>
      <AlertDescription>{!activity.leftDevice ? "Nothing was sent to the provider. No remote receipt is needed." : activity.verified === false ? "Receipt audit failed. Content may already have reached the client and cannot be retracted. See the recorded reason below." : receiptVerified ? "The signed receipt matches the request and response bytes recorded by the verifier." : "No successful verification result is recorded for this request."}</AlertDescription>
    </Alert>
    <dl className="grid grid-cols-[82px_minmax(0,1fr)] gap-x-4 gap-y-4.5 text-muted-foreground max-[440px]:grid-cols-1 max-[440px]:gap-y-1 [&_dd]:min-w-0 [&_dd]:wrap-anywhere [&_dd]:select-text [&_dd>code]:block [&_dt]:font-semibold">
      <dt>Request</dt>
      <dd>
        {agentName(activity.agent)} <code>{activity.method} {activity.path}</code>
      </dd>
      {activity.model && <><dt>Model</dt><dd><code>{activity.model}</code></dd></>}
      <dt>Outcome</dt>
      <dd>
        <StateLabel tone={outcome.tone} text={outcome.label} />
        {failed && <> HTTP {activity.status}</>}
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
    {activity.detail && <ProofNote title="Verification details"><p className="whitespace-pre-wrap wrap-break-word">{activity.detail}</p></ProofNote>}
    {activity.leftDevice && <ProofNote title="What the proof checks">
      <p>The verifier checks the request digest, the service signature against its attested keyset, and the response digest. This verifies the exchanged data, not answer accuracy.</p>
    </ProofNote>}
    </>
  );
}

function ProofNote({ title, children }: React.PropsWithChildren<{ title: string }>): React.JSX.Element {
  const titleId = useId();
  return <section className="grid gap-1.5 border-t pt-3.5 text-xs" aria-labelledby={titleId}>
    <h3 id={titleId} className="font-semibold">{title}</h3>
    <div className="text-muted-foreground">{children}</div>
  </section>;
}

/** Refreshes the record on open: its receipt may have been verified since the list loaded. */
export function UsageProofDialog({ activity: listed, ...control }: { activity: RequestActivity } & DialogControl): React.JSX.Element {
  const { data: activity = listed, error } = useQuery({
    queryKey: ["usage-record", listed.id], queryFn: () => desktopApi.getUsageRecord(listed.id), initialData: listed,
  });
  return (
    <AppDialog {...control} title="Usage proof" description={formatTimestamp(activity.at * 1_000, true)} className="sm:max-w-xl">
      {error && <Alert variant="destructive"><AlertTitle>Could not refresh this record</AlertTitle><AlertDescription>{errorMessage(error)}</AlertDescription></Alert>}
      <div className="-mx-6 flex min-h-0 flex-col gap-5 overflow-y-auto px-6"><Evidence activity={activity} />{activity.receiptId && <SignedReceipt recordId={activity.id} />}</div>
      <DoneFooter />
    </AppDialog>
  );
}

/**
 * A receipt document indented for reading. Parsing keeps every string and
 * safe integer exactly, so only whitespace changes; a document with any other
 * number (the verifier accepts any 64-bit integer) is shown as returned.
 */
function readableReceipt(receipt: string): string {
  let exact = true;
  const document: unknown = JSON.parse(receipt, (_key, value: unknown) => {
    if (typeof value === "number" && !Number.isSafeInteger(value)) exact = false;
    return value;
  });
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
    <FieldError>{copyError && `Could not copy the signed receipt. ${errorMessage(copyError)}`}</FieldError>
    <CodeBlock role="region" tabIndex={0} aria-labelledby={titleId}>{readable}</CodeBlock>
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
