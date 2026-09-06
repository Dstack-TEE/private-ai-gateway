import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";
import { differenceInCalendarDays, eachDayOfInterval, eachMonthOfInterval, format, parseISO, startOfDay, subDays } from "date-fns";
import type { UsagePage } from "../../shared/contracts";
import { ChartContainer, ChartTooltip, ChartTooltipContent, ChartLegend, ChartLegendContent, type ChartConfig } from "./ui/chart";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "./ui/tabs";

export type UsageMetric = "tokens" | "cost" | "requests";

export function modelChartData(page: Pick<UsagePage, "modelSeries" | "series"> & Partial<Pick<UsagePage, "models">>, range: string, metric: UsageMetric, bounds?: { start?: Date; end?: Date }) {
  const amount = (point: UsagePage["modelSeries"][number]) => metric === "cost" ? point.costUsd : metric === "tokens" ? point.tokens : point.requests;
  const totals = new Map<string | null, number>();
  for (const point of page.modelSeries) totals.set(point.model, (totals.get(point.model) ?? 0) + point.tokens);
  const models = [...totals.keys()].sort((a, b) => (totals.get(b) ?? 0) - (totals.get(a) ?? 0) || (a ?? "").localeCompare(b ?? ""));
  const colorDomain = [...new Set([...(page.models ?? []), ...models.filter((model) => model !== null)])].sort();
  const series = models.slice(0, 10).map((model, index) => ({ key: `model${index}`, label: model ?? "Unknown model", color: model === null ? "var(--muted-foreground)" : `var(--chart-${colorDomain.indexOf(model) % 10 + 1})` }));
  if (models.length > 10) series.push({ key: "other", label: "Other", color: "var(--muted-foreground)" });
  const modelKeys = new Map(models.map((model, index) => [model, index < 10 ? `model${index}` : "other"]));
  const today = startOfDay(new Date());
  const fixed = range === "24h" ? 1 : range === "7d" ? 7 : range === "30d" ? 30 : range === "90d" ? 90 : undefined;
  const first = page.series[0]?.day;
  const start = bounds?.start ?? (fixed ? subDays(today, fixed - 1) : first ? parseISO(first) : today);
  const end = bounds?.end ?? today;
  const monthly = differenceInCalendarDays(end, start) >= 90;
  const rows = new Map<string, { period: string; values: Record<string, number> }>();
  for (const date of (monthly ? eachMonthOfInterval : eachDayOfInterval)({ start, end })) {
    const period = format(date, monthly ? "yyyy-MM" : "yyyy-MM-dd");
    if (!rows.has(period)) rows.set(period, { period, values: Object.fromEntries(series.map((entry) => [entry.key, 0])) });
  }
  for (const point of page.modelSeries) {
    const row = rows.get(monthly ? point.day.slice(0, 7) : point.day);
    if (!row) continue;
    const key = modelKeys.get(point.model);
    if (!key) continue;
    row.values[key] = (row.values[key] ?? 0) + amount(point);
  }
  const data: Array<{ period: string } & Record<string, string | number>> = [...rows.values()].map(({ period, values }) => ({ period, ...values }));
  return { rows: data, series, monthly };
}

export function UsageChart({ page, loading, range, bounds, metric, onMetric }: {
  page?: UsagePage; loading: boolean; range: string; bounds?: { start?: Date; end?: Date }; metric: UsageMetric; onMetric(metric: UsageMetric): void;
}): React.JSX.Element {
  if (!page) return <figure className="usage-chart" aria-busy={loading}><div className="empty-state">{loading ? "Loading usage…" : "Usage data unavailable."}</div></figure>;
  const { rows, series, monthly } = modelChartData(page, range, metric, bounds);
  // Labels only: colors use existing CSS variables on Bars. ChartStyle emits no
  // dynamic style tag, preserving the desktop's restrictive production CSP.
  const config: ChartConfig = Object.fromEntries(series.map(({ key, label }) => [key, { label }]));
  const formatValue = (value: number) => metric === "cost"
    ? new Intl.NumberFormat(undefined, { style: "currency", currency: "USD", maximumFractionDigits: 6 }).format(value)
    : value.toLocaleString();
  return <figure className="usage-chart" aria-label={`${metric} usage by model${monthly ? " per month" : " per day"}`}>
    <Tabs value={metric} onValueChange={(value) => { if (value === "tokens" || value === "cost" || value === "requests") onMetric(value); }}>
      <TabsList aria-label="Chart metric">
        <TabsTrigger value="tokens">Tokens</TabsTrigger><TabsTrigger value="cost">Cost</TabsTrigger><TabsTrigger value="requests">Requests</TabsTrigger>
      </TabsList>
      <TabsContent value={metric}>
        <ChartContainer config={config} className="h-72 w-full aspect-auto">
          <BarChart accessibilityLayer data={rows} margin={{ top: 8, right: 4, left: 0, bottom: 0 }}>
            <CartesianGrid vertical={false} />
            <XAxis dataKey="period" tickLine={false} axisLine={false} minTickGap={28} tickFormatter={(value: string) => monthly ? value : value.slice(5)} />
            <YAxis width={48} tickLine={false} axisLine={false} tickFormatter={(value: number) => metric === "cost" ? `$${new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)}` : new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)} />
            <ChartTooltip content={<ChartTooltipContent className="max-w-72" formatter={(value, name, item) => <div className="grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2"><span className="size-2 shrink-0 rounded-full" style={{ backgroundColor: item.color }} /><span className="truncate text-muted-foreground" title={String(config[String(name)]?.label ?? name)}>{config[String(name)]?.label}</span><span className="font-mono">{formatValue(Number(value))}</span></div>} />} />
            <ChartLegend content={<ChartLegendContent className="max-h-24 flex-wrap justify-start overflow-y-auto [&>div]:max-w-full [&>div]:break-all" />} />
            {series.map(({ key, color }) => <Bar key={key} dataKey={key} name={key} stackId="models" fill={color} maxBarSize={48} isAnimationActive={false} />)}
          </BarChart>
        </ChartContainer>
        {page.summary.requests === 0 && <p className="text-sm text-muted-foreground">No usage in this range.</p>}
        <table className="sr-only" aria-label="Usage by model">
          <thead><tr><th>Period</th>{series.map((entry) => <th key={entry.key}>{entry.label}</th>)}</tr></thead>
          <tbody>{rows.map((row) => <tr key={row.period}><th>{row.period}</th>{series.map((entry) => <td key={entry.key}>{formatValue(Number(row[entry.key] ?? 0))}</td>)}</tr>)}</tbody>
        </table>
      </TabsContent>
    </Tabs>
  </figure>;
}
