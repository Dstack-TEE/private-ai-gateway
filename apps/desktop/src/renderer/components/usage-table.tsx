import { useMemo } from "react";
import { rowPaginationFeature, tableFeatures, useTable, type ColumnDef } from "@tanstack/react-table";
import type { RequestActivity } from "../../shared/contracts";
import { agentName, currency, formatTokens, outcomeOf, usageTokens } from "../lib/usage-presentation";
import { StateLabel } from "./state-label";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";

const features = tableFeatures({ rowPaginationFeature });

function TokenDetails({ item }: { item: RequestActivity }) {
  const total = usageTokens(item);
  const counts = [["Input", item.inputTokens], ["Output", item.outputTokens], ["Cache read", item.cacheReadTokens], ["Cache write", item.cacheWriteTokens]] as const;
  return <Popover>
    <PopoverTrigger render={<Button variant="ghost" size="sm" className="h-auto px-0 tabular-nums underline decoration-dotted underline-offset-4" aria-label="Token details" title="Token details" />}>
      {total === undefined ? "—" : formatTokens(total)}
    </PopoverTrigger>
    <PopoverContent align="end" aria-label="Token details">
      <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 text-sm tabular-nums">
        {counts.map(([label, count]) => <div key={label} className="contents"><dt className="text-muted-foreground">{label}</dt><dd className="text-right">{count === undefined ? "—" : count.toLocaleString()}</dd></div>)}
      </dl>
    </PopoverContent>
  </Popover>;
}

export function UsageTable({ items, loading, pageIndex, pageSize, total, onInspect }: {
  items: RequestActivity[]; loading: boolean; pageIndex: number; pageSize: number; total: number; onInspect(item: RequestActivity): void;
}) {
  const columns = useMemo<ColumnDef<typeof features, RequestActivity>[]>(() => [
    { id: "time", header: "Time", cell: ({ row }) => {
      const date = new Date(row.original.at * 1000);
      return <time dateTime={date.toISOString()} title={date.toLocaleString()} className="block tabular-nums"><span className="block">{date.toLocaleTimeString()}</span><span className="block text-xs text-muted-foreground">{date.toLocaleDateString()}</span></time>;
    } },
    { id: "agent", header: "Agent", cell: ({ row }) => <Button variant="link" className="h-auto p-0 text-left" onClick={() => onInspect(row.original)} aria-label={`${agentName(row.original.agent)}, ${outcomeOf(row.original).label}, ${row.original.model ?? row.original.path}. View proof`}>{agentName(row.original.agent)}</Button> },
    { accessorKey: "model", header: "Model", cell: ({ row }) => <span className="block max-w-48 truncate font-mono text-xs" title={row.original.model ?? row.original.path}>{row.original.model ?? row.original.path}</span> },
    { id: "tokens", header: "Tokens", cell: ({ row }) => <TokenDetails item={row.original} /> },
    { accessorKey: "costUsd", header: "Cost", cell: ({ row }) => <span className="block text-right tabular-nums">{row.original.costUsd === undefined ? "—" : currency(row.original.costUsd)}</span> },
    { id: "outcome", header: "Result", cell: ({ row }) => { const outcome = outcomeOf(row.original); return <StateLabel tone={outcome.tone} icon={outcome.icon} text={outcome.label} />; } },
  ], [onInspect]);
  const table = useTable({ features, data: items, columns, getRowId: (item) => item.id, manualPagination: true, rowCount: total, state: { pagination: { pageIndex, pageSize } } });
  return <div className="overflow-hidden rounded-2xl border" aria-busy={loading}>
    <Table aria-label="Usage history">
      <TableHeader>{table.getHeaderGroups().map((group) => <TableRow key={group.id}>{group.headers.map((header) => <TableHead key={header.id} className={header.id === "costUsd" || header.id === "tokens" ? "text-right" : undefined}><table.FlexRender header={header} /></TableHead>)}</TableRow>)}</TableHeader>
      <TableBody>
        {table.getRowModel().rows.map((row) => <TableRow key={row.id} className="cursor-pointer" onClick={(event) => {
          if (event.target instanceof Element && event.target.closest("button, a, [role=dialog]")) return;
          onInspect(row.original);
        }}>{row.getAllCells().map((cell) => <TableCell key={cell.id} className={cell.column.id === "tokens" ? "text-right" : undefined}><table.FlexRender cell={cell} /></TableCell>)}</TableRow>)}
        {items.length === 0 && <TableRow><TableCell colSpan={columns.length} className="h-24 text-center text-muted-foreground">{loading ? "Loading usage history…" : "No saved usage matches these filters."}</TableCell></TableRow>}
      </TableBody>
    </Table>
  </div>;
}
