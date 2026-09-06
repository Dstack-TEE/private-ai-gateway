import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";
import type { UsagePage } from "../../shared/contracts";
import { ChartContainer, ChartTooltip, ChartTooltipContent, ChartLegend, ChartLegendContent, type ChartConfig } from "./ui/chart";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "./ui/tabs";

export type UsageMetric = "tokens" | "cost" | "requests";

function dayKey(date: Date): string {
  return [date.getFullYear(), String(date.getMonth() + 1).padStart(2, "0"), String(date.getDate()).padStart(2, "0")].join("-");
}

export function modelChartData(page: Pick<UsagePage, "modelSeries" | "series">, range: string, metric: UsageMetric) {
  const amount = (point: UsagePage["modelSeries"][number]) => metric === "cost" ? point.costUsd : metric === "tokens" ? point.tokens : point.requests;
  const models = [...new Set(page.modelSeries.map((point) => point.model))].sort((a, b) => (a ?? "").localeCompare(b ?? ""));
  const series = models.map((model, index) => ({ key: `model${index}`, label: model ?? "Unknown model", color: `var(--chart-${index % 5 + 1})` }));
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const fixed = range === "24h" ? 1 : range === "7d" ? 7 : range === "30d" ? 30 : undefined;
  const first = page.series[0]?.day;
  const start = fixed ? new Date(today.getFullYear(), today.getMonth(), today.getDate() - fixed + 1)
    : first ? new Date(`${first}T00:00:00`) : today;
  const days: string[] = [];
  for (const day = new Date(start); day <= today; day.setDate(day.getDate() + 1)) days.push(dayKey(day));
  const monthly = days.length > 90;
  const rows = new Map<string, { period: string; values: Record<string, number> }>();
  for (const day of days) {
    const period = monthly ? day.slice(0, 7) : day;
    if (!rows.has(period)) rows.set(period, { period, values: Object.fromEntries(series.map((entry) => [entry.key, 0])) });
  }
  for (const point of page.modelSeries) {
    const row = rows.get(monthly ? point.day.slice(0, 7) : point.day);
    if (!row) continue;
    const key = `model${models.indexOf(point.model)}`;
    row.values[key] = (row.values[key] ?? 0) + amount(point);
  }
  const data: Array<{ period: string } & Record<string, string | number>> = [...rows.values()].map(({ period, values }) => ({ period, ...values }));
  return { rows: data, series, monthly };
}

export function UsageChart({ page, loading, range, metric, onMetric }: {
  page?: UsagePage; loading: boolean; range: string; metric: UsageMetric; onMetric(metric: UsageMetric): void;
}): React.JSX.Element {
  if (!page) return <figure className="usage-chart" aria-busy={loading}><div className="empty-state">{loading ? "Loading usage…" : "Usage data unavailable."}</div></figure>;
  const { rows, series, monthly } = modelChartData(page, range, metric);
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
            <ChartTooltip content={<ChartTooltipContent formatter={(value, name) => <div className="flex w-full items-center justify-between gap-4"><span className="text-muted-foreground">{config[String(name)]?.label}</span><span className="font-mono">{formatValue(Number(value))}</span></div>} />} />
            <ChartLegend content={<ChartLegendContent className="flex-wrap justify-start [&>div]:max-w-full [&>div]:break-all" />} />
            {series.map(({ key, color }) => <Bar key={key} dataKey={key} name={key} stackId="models" fill={color} isAnimationActive={false} />)}
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
