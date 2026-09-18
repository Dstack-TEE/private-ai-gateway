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
