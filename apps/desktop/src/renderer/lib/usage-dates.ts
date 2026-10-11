import { addDays, format, startOfDay, subDays } from "date-fns";

export const DATE_PRESETS = {
  "24h": "Today",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
  "90d": "Last 90 days",
  all: "All time",
} as const;

export type UsageDateSelection = { preset: keyof typeof DATE_PRESETS }
  | { preset: "custom"; from: Date; to: Date };

export function usageDateBounds(selection: UsageDateSelection, now = new Date()) {
  if (selection.preset === "all") return { since: undefined, until: undefined, start: undefined, end: undefined };
  const end = startOfDay(selection.preset === "custom" ? selection.to : now);
  const days = selection.preset === "24h" ? 1 : selection.preset === "7d" ? 7 : selection.preset === "30d" ? 30 : 90;
  const start = selection.preset === "custom" ? startOfDay(selection.from) : subDays(end, days - 1);
  return { start, end, since: Math.floor(start.getTime() / 1000), until: Math.floor(addDays(end, 1).getTime() / 1000) };
}

export function usageDateLabel(selection: UsageDateSelection) {
  return selection.preset === "custom"
    ? `${format(selection.from, "MMM d, yyyy")} - ${format(selection.to, "MMM d, yyyy")}`
    : DATE_PRESETS[selection.preset];
}

export const USAGE_PAGE_SIZES = [20, 50, 100] as const;

/** Usage filters kept in the page URL; a custom range is a `from`/`to` pair of local days. */
export interface UsageSearch {
  agent?: string;
  model?: string;
  range?: keyof typeof DATE_PRESETS;
  from?: string;
  to?: string;
  rows?: (typeof USAGE_PAGE_SIZES)[number];
}

export const USAGE_SEARCH_DEFAULTS = { range: "7d", rows: 20 } satisfies UsageSearch;

export function validateUsageSearch(search: Record<string, unknown>): UsageSearch {
  const from = typeof search.from === "string" ? parseDay(search.from) : undefined;
  const to = typeof search.to === "string" ? parseDay(search.to) : undefined;
  const custom = from && to && from <= to;
  return {
    agent: text(search.agent),
    model: text(search.model),
    range: !custom && isPreset(search.range) ? search.range : undefined,
    from: custom ? format(from, DAY) : undefined,
    to: custom ? format(to, DAY) : undefined,
    rows: USAGE_PAGE_SIZES.find((size) => size === search.rows),
  };
}

export function usageDateSelection(search: UsageSearch): UsageDateSelection {
  const from = search.from ? parseDay(search.from) : undefined;
  const to = search.to ? parseDay(search.to) : undefined;
  return from && to ? { preset: "custom", from, to } : { preset: search.range ?? USAGE_SEARCH_DEFAULTS.range };
}

export function usageDateSearch(selection: UsageDateSelection): Pick<UsageSearch, "range" | "from" | "to"> {
  return selection.preset === "custom"
    ? { range: undefined, from: format(selection.from, DAY), to: format(selection.to, DAY) }
    : { range: selection.preset, from: undefined, to: undefined };
}

const DAY = "yyyy-MM-dd";

function parseDay(value: string): Date | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return undefined;
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return format(date, DAY) === value ? date : undefined;
}

function isPreset(value: unknown): value is keyof typeof DATE_PRESETS {
  return typeof value === "string" && Object.hasOwn(DATE_PRESETS, value);
}

// Search values are JSON-decoded, so an identifier such as `123` arrives as a number.
function text(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : typeof value === "number" ? String(value) : undefined;
}
