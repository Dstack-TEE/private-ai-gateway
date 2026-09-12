import React, { useEffect, useState } from "react";
import { RefreshCw, ShieldCheck, ShieldX } from "lucide-react";
import { SwitchControl } from "./controls";
import type { GatewayState } from "../../shared/contracts";
import { isProtected } from "../lib/protection";

export function ProtectionStatus({ state, label }: { state: GatewayState; label: string }): React.JSX.Element {
  const active = isProtected(state);
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
  return (
    <span className="protection-status inline-flex items-center justify-center gap-1.25 max-w-full flex-wrap [&_>_svg]:flex-none">
      {active ? <ShieldCheck size={14} aria-hidden="true" /> : state.reconnecting ? <RefreshCw size={14} aria-hidden="true" /> : <ShieldX size={14} aria-hidden="true" />}
      <span aria-live="polite">{label}</span>
      {elapsed !== undefined && <time className="protection-duration w-[8ch] font-medium text-xs leading-4.5 font-mono tabular-nums text-muted-foreground whitespace-nowrap" dateTime={`PT${seconds}S`} aria-label={`Session elapsed ${elapsed}`}>{elapsed}</time>}
    </span>
  );
}

export function ProtectedControl({
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  compact = false,
  iconOnly = false,
  onToggle,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  compact?: boolean;
  iconOnly?: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const protectionStarting = busy && !state.configurationVerification;
  const checked = running || protectionStarting || Boolean(state.reconnecting);
  const label = busy
    ? state.configurationVerification ? "Verifying configuration" : "Cancel protection start"
    : state.reconnecting ? "Cancel reconnection" : running ? "Stop protection" : "Start protection";
  return (
    <div className={`protected-control flex items-center gap-2.5 text-xs font-semibold [&.is-compact]:p-0 max-[440px]:[&.is-compact]:gap-1.5 max-[440px]:[&.is-compact]:pl-1.75 max-[440px]:[&.is-compact]:text-xs ${compact ? "is-compact" : ""} ${iconOnly && !compact ? "is-icon-only mt-0.75" : ""}`}>
      {!iconOnly && <span>Protected</span>}
      {developmentMode && !compact && <span className="dev-mode-label text-warning text-xs font-semibold">Dev mode</span>}
      <SwitchControl
        size="default"
        checked={checked}
        label={label}
        disabled={(busy && state.configurationVerification) || (endpointDown && !checked)}
        developmentMode={developmentMode}
        onToggle={onToggle}
      />
    </div>
  );
}
