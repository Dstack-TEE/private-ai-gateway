import { useMemo } from "react";
import { rowPaginationFeature, tableFeatures, useTable, type ColumnDef } from "@tanstack/react-table";
import type { RequestActivity } from "../../shared/contracts";
import { agentName, currency, formatTokens, outcomeOf, usageTokens } from "../lib/usage-presentation";
import { StateLabel } from "./state-label";
import { Button } from "./ui/button";
import { Hint } from "./hint";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";

const features = tableFeatures({ rowPaginationFeature });

function TokenDetails({ item }: { item: RequestActivity }) {
  const total = usageTokens(item);
  const counts = [["Input", item.inputTokens], ["Output", item.outputTokens], ["Cache read", item.cacheReadTokens], ["Cache write", item.cacheWriteTokens]] as const;
  return <Hint content={
      <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 tabular-nums">
        {counts.map(([label, count]) => <div key={label} className="contents"><dt>{label}</dt><dd className="text-right">{count === undefined ? "—" : count.toLocaleString()}</dd></div>)}
      </dl>
  }><span tabIndex={0} aria-label="Token details" className="tabular-nums underline decoration-dotted underline-offset-4">{total === undefined ? "—" : formatTokens(total)}</span></Hint>;
}

export function UsageTable({ items, loading, pageIndex, pageSize, total, onInspect }: {
  items: RequestActivity[]; loading: boolean; pageIndex: number; pageSize: number; total: number; onInspect(item: RequestActivity): void;
}) {
  const columns = useMemo<ColumnDef<typeof features, RequestActivity>[]>(() => [
    { id: "time", header: "Time", cell: ({ row }) => {
      const date = new Date(row.original.at * 1000);
      return <time dateTime={date.toISOString()} className="block tabular-nums"><span className="block">{date.toLocaleTimeString()}</span><span className="block text-xs text-muted-foreground">{date.toLocaleDateString()}</span></time>;
    } },
    { id: "agent", header: "Agent", cell: ({ row }) => <Button variant="ghost" className="h-auto p-0 text-left" onClick={() => onInspect(row.original)} aria-label={`${agentName(row.original.agent)}, ${outcomeOf(row.original).label}, ${row.original.model ?? row.original.path}. View proof`}>{agentName(row.original.agent)}</Button> },
    { accessorKey: "model", header: "Model", cell: ({ row }) => <Hint content={row.original.model ?? row.original.path}><span className="block max-w-48 truncate font-mono text-xs">{row.original.model ?? row.original.path}</span></Hint> },
    { id: "tokens", header: "Tokens", cell: ({ row }) => <TokenDetails item={row.original} /> },
    { accessorKey: "costUsd", header: "Cost", cell: ({ row }) => <span className="block text-right tabular-nums">{row.original.costUsd === undefined ? "—" : currency(row.original.costUsd)}</span> },
    { id: "outcome", header: "Result", cell: ({ row }) => { const outcome = outcomeOf(row.original); return <StateLabel tone={outcome.tone} text={outcome.label} />; } },
  ], [onInspect]);
  const table = useTable({ features, data: items, columns, getRowId: (item) => item.id, manualPagination: true, rowCount: total, state: { pagination: { pageIndex, pageSize } } });
  return <Table aria-label="Usage history" aria-busy={loading}>
      <TableHeader>{table.getHeaderGroups().map((group) => <TableRow key={group.id}>{group.headers.map((header) => <TableHead key={header.id} className={header.id === "costUsd" || header.id === "tokens" ? "text-right" : undefined}><table.FlexRender header={header} /></TableHead>)}</TableRow>)}</TableHeader>
      <TableBody>
        {table.getRowModel().rows.map((row) => <TableRow key={row.id} className="cursor-pointer" onClick={(event) => {
          if (event.target instanceof Element && event.target.closest("button, a, [role=dialog]")) return;
          onInspect(row.original);
        }}>{row.getAllCells().map((cell) => <TableCell key={cell.id} className={cell.column.id === "tokens" ? "text-right" : undefined}><table.FlexRender cell={cell} /></TableCell>)}</TableRow>)}
        {items.length === 0 && <TableRow><TableCell colSpan={columns.length} className="h-24 text-center text-muted-foreground">{loading ? "Loading usage history…" : "No saved usage matches these filters."}</TableCell></TableRow>}
      </TableBody>
    </Table>;
}
