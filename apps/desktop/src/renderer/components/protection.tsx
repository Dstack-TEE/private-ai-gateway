import React, { useEffect, useState } from "react";
import { RefreshCw, ShieldCheck, ShieldX } from "lucide-react";
import { SwitchControl } from "./controls";
import type { AppState } from "../../shared/contracts";
import { cn } from "../lib/utils";

/** The protection title with the session's elapsed time: large in the overview card, small in the page header. */
export function ProtectionStatus({ state, variant }: { state: AppState; variant: "card" | "header" }): React.JSX.Element {
  const { phase, title } = state.protection;
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
  const card = variant === "card";
  const iconSize = card ? 24 : 14;
  return (
    <span className={cn("grid max-w-full items-center", card
      ? "grid-cols-[24px_minmax(0,1fr)] justify-start gap-x-1.5 gap-y-1 text-2xl font-semibold"
      : "grid-cols-[14px_auto] justify-center justify-items-end gap-x-1.25")}>
      {active ? <ShieldCheck size={iconSize} aria-hidden="true" /> : phase === "reconnecting" ? <RefreshCw size={iconSize} aria-hidden="true" /> : <ShieldX size={iconSize} aria-hidden="true" />}
      <span aria-live="polite">{title}</span>
      {elapsed !== undefined && <time className={cn("w-[8ch] font-mono text-xs leading-4.5 whitespace-nowrap text-muted-foreground tabular-nums", card ? "col-start-2 font-normal" : "col-span-full font-medium")} dateTime={`PT${seconds}S`} aria-label={`Session elapsed ${elapsed}`}>{elapsed}</time>}
    </span>
  );
}

/** The protection switch; its action is the tray's protection item. */
export function ProtectedControl({
  state,
  pending,
  compact = false,
  className,
  onToggle,
}: {
  state: AppState;
  /** A start or stop is in flight. */
  pending: boolean;
  /** The page header's control, without the development mode label. */
  compact?: boolean;
  className?: string;
  onToggle(): void;
}): React.JSX.Element {
  const { action } = state.protection;
  const developmentMode = !state.config.requireProductionOs;
  return (
    <div className={cn("flex items-center gap-2.5 text-xs font-semibold", compact && "max-[440px]:gap-1.5 max-[440px]:pl-1.75", className)}>
      {developmentMode && !compact && <span className="text-xs font-semibold text-warning">Dev mode</span>}
      <SwitchControl
        size="default"
        className={compact ? undefined : "transition-colors duration-200 ease-out motion-reduce:transition-none"}
        checked={action.operation === "stop"}
        label={action.label}
        disabled={!action.enabled || pending}
        aria-busy={pending}
        developmentMode={developmentMode}
        onToggle={onToggle}
      />
    </div>
  );
}
