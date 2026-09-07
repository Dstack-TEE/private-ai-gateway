import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";
import { ChartContainer, ChartTooltip, ChartTooltipContent, ChartLegend, ChartLegendContent, type ChartConfig } from "./ui/chart";
import type { modelChartData, UsageMetric } from "./usage-chart";

export default function UsagePlot({ rows, series, monthly, metric, formatValue }: ReturnType<typeof modelChartData> & { metric: UsageMetric; formatValue(value: number): string }) {
  // Labels only: colors remain CSS variables, without dynamic style tags under CSP.
  const config: ChartConfig = Object.fromEntries(series.map(({ key, label }) => [key, { label }]));
  return <ChartContainer config={config} className="h-72 w-full aspect-auto">
    <BarChart accessibilityLayer data={rows} margin={{ top: 8, right: 4, left: 0, bottom: 0 }}>
      <CartesianGrid vertical={false} />
      <XAxis dataKey="period" tickLine={false} axisLine={false} minTickGap={28} tickFormatter={(value: string) => monthly ? value : value.slice(5)} />
      <YAxis width={48} tickLine={false} axisLine={false} tickFormatter={(value: number) => metric === "cost" ? `$${new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)}` : new Intl.NumberFormat(undefined, { notation: "compact" }).format(value)} />
      <ChartTooltip content={<ChartTooltipContent className="max-w-72" formatter={(value, name, item) => <div className="grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2"><span className="size-2 shrink-0 rounded-full" style={{ backgroundColor: item.color }} /><span className="truncate text-muted-foreground" title={String(config[String(name)]?.label ?? name)}>{config[String(name)]?.label}</span><span className="font-mono">{formatValue(Number(value))}</span></div>} />} />
      <ChartLegend content={<ChartLegendContent className="max-h-24 flex-wrap justify-start overflow-y-auto [&>div]:max-w-full [&>div]:break-all" />} />
      {series.map(({ key, color }) => <Bar key={key} dataKey={key} name={key} stackId="models" fill={color} maxBarSize={48} isAnimationActive={false} />)}
    </BarChart>
  </ChartContainer>;
}
