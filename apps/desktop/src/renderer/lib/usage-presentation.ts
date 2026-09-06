import { Ban, LoaderCircle, ShieldCheck, ShieldX, TriangleAlert } from "lucide-react";
import type { RequestActivity } from "../../shared/contracts";
export type Tone = "success" | "warning" | "danger" | "neutral";

export function outcomeOf(activity: RequestActivity): { label: string; tone: Tone; icon: typeof ShieldCheck } {
  if (!activity.leftDevice) return { label: "Blocked locally", tone: "neutral", icon: Ban };
  if (activity.verified === false) return { label: "Proof failed", tone: "danger", icon: TriangleAlert };
  if (activity.status < 200 || activity.status >= 300) return { label: "Upstream failed", tone: "danger", icon: TriangleAlert };
  if (activity.verified === true) return { label: "Protected", tone: "success", icon: ShieldCheck };
  if (activity.receiptId) return { label: "Proof pending", tone: "warning", icon: LoaderCircle };
  return { label: "Proof unavailable", tone: "warning", icon: ShieldX };
}

export function agentName(id?: string): string {
  switch (id) {
    case "codex": return "Codex";
    case "claude-code": return "Claude Code";
    case "opencode": return "OpenCode";
    case "pi": return "Pi";
    case "hermes": return "Hermes Agent";
    case "local-tools": return "Local API";
    default: return id ?? "Unknown client";
  }
}

export function usageTokens(item: RequestActivity) {
  return item.inputTokens === undefined && item.outputTokens === undefined ? undefined : (item.inputTokens ?? 0) + (item.outputTokens ?? 0);
}

export function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(tokens >= 10_000_000 ? 0 : 1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(tokens >= 100_000 ? 0 : 1)}K`;
  return tokens.toLocaleString();
}

export function currency(value: number): string {
  const digits = value > 0 && value < 0.01 ? 4 : 2;
  return new Intl.NumberFormat(undefined, { style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits }).format(value);
}
