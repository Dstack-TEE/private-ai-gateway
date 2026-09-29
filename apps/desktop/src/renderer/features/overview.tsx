import React, { memo } from "react";
import { ChevronDown, CircleHelp, Info, LoaderCircle, Plus, RefreshCw, Settings } from "lucide-react";
import { Link } from "@tanstack/react-router";
import { Button, buttonVariants } from "../components/ui/button";
import { StateLabel } from "../components/state-label";
import { currency, formatTokens } from "../lib/usage-presentation";
import { Card, CardHeader, CardTitle, CardDescription, CardAction, CardContent } from "../components/ui/card";
import { Separator } from "../components/ui/separator";
import { IconButton } from "../components/controls";
import type { UsageSummary } from "../../shared/contracts";
import { useShell } from "../lib/shell";
import { LocalApiPanel } from "./local-api";
import { EmptyState } from "../components/detail";
import { AgentRow } from "./agents";
import { UsageRow } from "./usage";
import { ProtectedControl, ProtectionStatus } from "../components/protection";
import { ServiceLogo } from "../components/brand";
import { AccountBalanceValue } from "../components/account-tools";
import { toneTextClass } from "../lib/tone";
import { activeProfile } from "../lib/protection";
import { cn } from "../lib/utils";

const TLS_TRACKS = [
  "17 03 03 00 f4   9f3a c1e0 7b42 d5a8 0e6f 2c91 4d17 e8b3 5a0c f9d2 61b7 a3e4 b8c5 0f2e 93d1 7a46 e5b0 1c8d",
  "17 03 03 03 1a   4d17 e8b3 5a0c f9d2 61b7 a3e4 b8c5 0f2e 93d1 7a46 e5b0 1c8d 2e7f a94b 6d03 c1e8 5f27 b6a9",
  "application_data   record_len 244   17 03 03 00 f4   6d03 c1e8 5f27 b6a9 70d2 3b8e c4f1 a90d 1e6c 8b35 e1a7 5c09 f38d 2b64",
  "17 03 03 01 6c   e1a7 5c09 f38d 2b64 d0e7 4a1f 6b0c 8e52 1d9f a7c3 3e08 f5b4 c3d6 4f81 b2a0 7e95 0d1b 9c6e",
  "17 03 03 00 5e   b2a0 7e95 0d1b 9c6e 18e4 a0f7 5b3c d29a 6c04 e7f1 8d2b f6c0 3a17 e94d b5c8 02a6 5f7e 1b93",
  "application_data   record_len 794   17 03 03 03 1a   b5c8 02a6 5f7e 1b93 c80a d4e2 76b1 3d0c 9e21 4fb7 a6d5 0c83 e2f9 71b4",
  "17 03 03 02 48   9e21 4fb7 a6d5 0c83 e2f9 71b4 5d0e 8ac6 3f92 b7e0 6a1d c95f 2d38 f04b 81c7 e6a2 5b9d 1f74",
  "17 03 03 00 91   81c7 e6a2 5b9d 1f74 c0e3 a8d6 4e27 9b1c d5f0 3c68 7e4a f2c4 0b9e 6d17 a3e8 5c02 e9b1 4d7f",
  "17 03 03 01 d0   a3e8 5c02 e9b1 4d7f 8a36 1e0c b5d9 7f23 c6a4 0e81 d3b7 2a5c 9f6e 4b10 c7d2 3e5a 90f4 1b6c",
  "application_data   record_len 152   17 03 03 00 98   c7d2 3e5a 90f4 1b6c 8d07 e2a9 5f31 b48e 7a0d 2c95 f6e3 41b8 d9c0 3f5e",
  "17 03 03 00 3c   7a0d 2c95 f6e3 41b8 d9c0 3f5e 8a2b 6e17 c4d8 0b93 5a6f e1d2 7c04 93ab 5e8f 21c6 d0a3 7b19",
];

export function OverviewPage(): React.JSX.Element {
  const shell = useShell();
  const { state, agents } = shell;
  const protectedNow = state.protection.phase === "protected";
  const localAvailable = protectedNow && Boolean(state.proxyUrl);
  const sessionShown = protectedNow || state.sessionActive || state.reconnecting;
  const recent = sessionShown ? state.activity.slice(0, 10) : [];
  const previewAgents = agents.accessStatus === "authorized"
    ? agents.agents.filter((agent) => agent.installed).slice(0, 4)
    : agents.agents.slice(0, 4);
  return (
    <div className="mx-auto flex min-h-full max-w-240 flex-col @container/overview">
      <div className="grid grid-cols-2 items-stretch gap-4 @max-[600px]/overview:grid-cols-1">
      <StatusSurface />
      <SessionSummary summary={state.sessionUsage} active={sessionShown} />
      </div>
      {/* Local API and Agents fill the first column beside Recent usage; one column stacks them in order. */}
      <div className="mt-4 grid grid-cols-2 grid-rows-[auto_auto] gap-4 @max-[600px]/overview:grid-cols-1">
        <OverviewModule title="Local API" description="Use private AI in your tools." titleAdornment={<IconButton variant="ghost" size="icon-xs" label="Local API examples" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "local-api-example" })}><CircleHelp aria-hidden="true" /></IconButton>} status={<StateLabel tone={localAvailable ? "success" : "neutral"} text={localAvailable ? "Available" : "Unavailable"} />} action={<IconButton label="Local API settings" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "local-api" })}><Settings size={16} /></IconButton>}>
          <LocalApiPanel />
        </OverviewModule>
        <OverviewModule stretch={false} title="Agents" description="Use private AI in your agents." action={agents.accessStatus !== "authorized" || agents.authorizing
          ? <Button type="button" variant="outline" size="sm" className="relative min-w-20" disabled={!agents.accessStatus || agents.authorizing} aria-busy={agents.authorizing} aria-label="Enable agent access" onClick={agents.requestAccess}>
              <span className={agents.authorizing ? "invisible" : undefined}>Enable</span>
              {agents.authorizing && <LoaderCircle aria-hidden="true" className="absolute animate-spin" />}
            </Button>
          : <Link to="/agents" className={cn(buttonVariants({ variant: "outline", size: "sm" }), "min-w-20")}>View All</Link>}>
          {/* Room for four compact agent rows: a 2rem mark, 1.25rem of padding and the border. */}
          <div className="grid grid-rows-[repeat(4,minmax(calc(2rem+1.25rem+2px),auto))] gap-3">
            {agents.accessStatus === "authorized" && !agents.agents.some((agent) => agent.installed) ? <EmptyState className="row-span-full" text={agents.problem ? "Agent detection unavailable" : "No agents detected"} />
              : previewAgents.map((agent) => (
              <AgentRow
                key={agent.id}
                agent={agent}
                compact
                detectionLabel={agents.detectionLabel}
                disabled={shell.applying || agents.controlsLocked || Boolean(agents.detectionLabel)}
              />
            ))}
          </div>
        </OverviewModule>
        <OverviewModule
          title="Recent usage"
          description="Latest requests in this session."
          action={<Link to="/usage" className={cn(buttonVariants({ variant: "outline", size: "sm" }))}>View All</Link>}
          scrollable
          className="col-start-2 row-[1/span_2] @max-[600px]/overview:col-auto @max-[600px]/overview:row-auto"
        >
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain" role="region" tabIndex={0} aria-label="Recent requests">
            {recent.length === 0 && (
              <EmptyState text={state.protection.action.operation === "stop" || state.sessionActive ? "No requests in this session yet." : "Start protection to begin a new session."} />
            )}
            {recent.map((item) => (
              <React.Fragment key={item.id}><UsageRow activity={item} onOpen={() => shell.openDialog({ kind: "usage-proof", activity: item })} /><Separator className="last:hidden" /></React.Fragment>
            ))}
          </div>
        </OverviewModule>
      </div>
    </div>
  );
}

function StatusSurface(): React.JSX.Element {
  const shell = useShell();
  const { state } = shell;
  const protection = state.protection;
  const protectedNow = protection.phase === "protected";
  const developmentMode = !state.config.requireProductionOs;
  const profile = activeProfile(state);
  return (
    <Card size="sm" role="region" className={cn(
      "relative isolate min-h-36 min-w-0 transition-colors duration-200 ease-out motion-reduce:transition-none",
      protectedNow && (developmentMode ? "ring-warning shadow-warning/10 dark:ring-warning" : "ring-primary shadow-primary/10 dark:ring-primary"),
    )} aria-label="Protection status">
      <TrackLayer active={protectedNow} />
      <CardContent className="relative z-2 grid w-full flex-1 grid-cols-[minmax(0,1fr)_44px] grid-rows-[auto_1fr] gap-x-3 gap-y-2">
        <div className={cn("col-start-1 row-start-1 min-w-0 transition-colors duration-200 ease-out motion-reduce:transition-none", toneTextClass[protection.tone])}>
          <ProtectionStatus state={state} variant="card" />
        </div>
        <div className="col-span-full row-start-2 flex min-w-0 items-center gap-2 self-end">
        {state.backendConnected === false ? <Button variant="outline" size="sm" disabled={shell.startingBackend} onClick={shell.startBackend}><RefreshCw className={shell.startingBackend ? "animate-control-spin" : undefined} aria-hidden="true" />{shell.startingBackend ? "Starting…" : "Start Background Service"}</Button> : <>
        {/* On the card's surface in both themes, without the outline button's light hover fill. */}
        <Button id="overview-profile" variant="outline" size="sm" className="w-[min(140px,100%)] min-w-0 bg-card hover:bg-card dark:bg-card" aria-label={profile ? `Profiles: ${profile.name}` : "Set Up Profile"} aria-haspopup="dialog" onClick={shell.openProfiles}>
          {profile ? <ServiceLogo provider={profile.provider} className="size-5" /> : <Plus aria-hidden="true" />}
          <span className="min-w-0 flex-1 truncate text-left">{profile?.name ?? "Set Up"}</span>
          {profile && <ChevronDown aria-hidden="true" />}
        </Button>
        {profile?.auth.kind === "oauth" && <AccountBalanceValue provider={profile.provider} target={{ kind: "profile", profileId: profile.id }} credentialRef={profile.credentialRef} enabled={protectedNow} />}
        <IconButton size="icon-sm" label="Privacy verification" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "privacy" })}><Info aria-hidden="true" /></IconButton>
        </>}
        </div>
        <ProtectedControl variant="card" className="col-start-2 row-start-1 min-h-8 self-start justify-self-end" state={state} pending={shell.protectionPending} onToggle={shell.toggleProtection} />
      </CardContent>
    </Card>
  );
}

const TrackLayer = memo(function TrackLayer({ active }: { active: boolean }): React.JSX.Element {
  return (
    <div className={cn("pointer-events-none absolute inset-0 z-1 grid grid-rows-11 items-center overflow-hidden py-2 text-[color-mix(in_srgb,var(--muted-foreground)_5%,var(--card))] [mask-image:linear-gradient(to_right,transparent,#000_12%,#000_88%,transparent)] transition-opacity duration-500 motion-reduce:transition-none", active ? "opacity-100" : "opacity-0")} aria-hidden="true">
      {TLS_TRACKS.map((line, index) => <TrackRow key={line} text={line} reverse={index % 2 === 1} running={active} />)}
    </div>
  );
});

function TrackRow({ text, reverse, running }: { text: string; reverse: boolean; running: boolean }): React.JSX.Element {
  return (
    <div className="flex min-w-0 items-center overflow-hidden font-mono text-xs leading-4.5 whitespace-nowrap">
      <div className={cn("flex w-max motion-reduce:animate-none", reverse ? "animate-track-right" : "animate-track-left", running ? "[animation-play-state:running]" : "[animation-play-state:paused]")}>
        <span className="flex-none pr-8">{text}</span><span className="flex-none pr-8">{text}</span>
      </div>
    </div>
  );
}

function OverviewModule({
  title,
  description,
  titleAdornment,
  status,
  action,
  scrollable = false,
  stretch = true,
  className,
  children,
}: React.PropsWithChildren<{
  title: string;
  description?: string;
  titleAdornment?: React.ReactNode;
  status?: React.ReactNode;
  action?: React.ReactNode;
  scrollable?: boolean;
  stretch?: boolean;
  className?: string;
}>): React.JSX.Element {
  return (
    <Card size="sm" className={cn("flex min-h-0 min-w-0 flex-col", stretch ? "h-full" : "h-auto self-start", className)}>
      <CardHeader className="flex-none items-center">
        <CardTitle className="flex flex-wrap items-center gap-2"><h2 className="text-base font-medium">{title}</h2>{titleAdornment}{status}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      <CardContent className={cn("flex min-w-0 flex-1 flex-col @container", scrollable ? "min-h-0 [contain:size] @max-[600px]/overview:h-80 @max-[600px]/overview:flex-none" : "shrink-0")}>{children}</CardContent>
    </Card>
  );
}

function SessionSummary({ summary, active }: { summary: UsageSummary; active: boolean }): React.JSX.Element {
  const totalTokens = summary.inputTokens + summary.outputTokens;
  return (
    <Card size="sm" role="region" className="min-h-36 min-w-0" aria-labelledby="session-usage-heading">
      <CardHeader><CardTitle><h2 id="session-usage-heading" className="text-base font-medium">Current session</h2></CardTitle></CardHeader>
    <CardContent className="mt-auto flex min-w-0 items-stretch gap-3" role="group" aria-label="Usage in this session">
      {[
        ["Requests", active ? summary.requests.toLocaleString() : "—"],
        ["Tokens", active ? formatTokens(totalTokens) : "—"],
        ["Estimated cost", active ? currency(summary.costUsd) : "—"],
      ].map(([label, value], index) => <React.Fragment key={label}>
        {index > 0 && <Separator orientation="vertical" className="h-auto self-stretch" />}
        <div className="flex min-w-0 flex-1 flex-col justify-between gap-2">
          <span className="text-xs text-muted-foreground">{label}</span>
          <strong className="truncate text-xl font-semibold tabular-nums">{value}</strong>
        </div>
      </React.Fragment>)}
    </CardContent>
    </Card>
  );
}
