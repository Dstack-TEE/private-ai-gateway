import React, { useEffect, useState } from "react";
import { RefreshCw, ShieldCheck, ShieldX } from "lucide-react";
import { SwitchControl } from "./controls";
import { StateLabel } from "./state-label";
import { toneTextClass } from "../lib/tone";
import { cn } from "../lib/utils";
import type { AppState } from "../../shared/contracts";

export function ProtectionStatus({ state }: { state: AppState }): React.JSX.Element {
  const { phase, title, tone } = state.protection;
  const active = phase === "protected";
  const since = active ? state.protectedSince : undefined;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === undefined) return;
    let timer: number | undefined;
    const syncVisibility = () => {
      window.clearInterval(timer);
      if (document.hidden) return;
      setNow(Date.now());
      timer = window.setInterval(() => setNow(Date.now()), 1_000);
    };
    document.addEventListener("visibilitychange", syncVisibility);
    syncVisibility();
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", syncVisibility);
    };
  }, [since]);
  const seconds = since === undefined ? undefined : Math.max(0, Math.floor(now / 1_000) - since);
  const elapsed = seconds === undefined ? undefined : [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60].map((value) => String(value).padStart(2, "0")).join(":");
  const Icon = active ? ShieldCheck : phase === "reconnecting" ? RefreshCw : ShieldX;
  return (
    <span className={cn("inline-flex min-w-0 flex-wrap items-center gap-x-1.5", toneTextClass[tone])}>
      <Icon size={14} aria-hidden="true" />
      <span aria-live="polite">{title}</span>
      {elapsed !== undefined && <time className="font-mono text-xs font-normal text-muted-foreground tabular-nums" dateTime={`PT${seconds}S`} aria-label={`Session elapsed ${elapsed}`}>{elapsed}</time>}
    </span>
  );
}

/** The protection switch; its action is the tray's protection item. */
export function ProtectedControl({
  state,
  pending,
  compact = false,
  onToggle,
}: {
  state: AppState;
  /** A start or stop is in flight. */
  pending: boolean;
  /** Leaves out the development OS label. */
  compact?: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const { action } = state.protection;
  return (
    <div className="flex items-center gap-2">
      {!state.config.requireProductionOs && !compact && <StateLabel tone="warning" text="Dev mode" />}
      <SwitchControl
        checked={action.operation === "stop"}
        label={action.label}
        disabled={!action.enabled || pending}
        aria-busy={pending}
        onToggle={onToggle}
      />
    </div>
  );
}
