import React, { memo } from "react";
import { ChevronDown, CircleHelp, Info, LoaderCircle, Plus, RefreshCw, Settings } from "lucide-react";
import { Link } from "@tanstack/react-router";
import { Button, buttonVariants } from "../components/ui/button";
import { StateLabel } from "../components/state-label";
import { currency, formatTokens } from "../lib/usage-presentation";
import { Card, CardHeader, CardTitle, CardDescription, CardAction, CardContent } from "../components/ui/card";
import { Separator } from "../components/ui/separator";
import { ItemGroup } from "../components/ui/item";
import { IconButton } from "../components/controls";
import type { UsageSummary } from "../../shared/contracts";
import { desktopApi } from "../lib/environment";
import { useShell } from "../lib/shell";
import { LocalApiPanel } from "./local-api";
import { EmptyState } from "../components/detail";
import { AgentRow } from "./agents";
import { UsageRow } from "./usage";
import { ProtectedControl, ProtectionStatus } from "../components/protection";
import { ServiceLogo } from "../components/brand";
import { AccountBalanceValue } from "../components/account-tools";
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
  const recent = protectedNow || state.sessionActive || state.reconnecting ? state.activity.slice(0, 10) : [];
  const agentDetectionLabel = agents.accessStatus === "authorized"
    ? undefined
    : agents.authorizing
      ? "Waiting for access"
      : agents.accessStatus
        ? "Access required"
        : "Checking access";
  const previewAgents = agents.accessStatus === "authorized"
    ? agents.agents.filter((agent) => agent.installed).slice(0, 3)
    : agents.agents.slice(0, 3);
  return (
    <div className="@container/overview mx-auto flex max-w-240 flex-col gap-4">
      <div className="grid grid-cols-2 gap-4 @max-[600px]/overview:grid-cols-1">
        <StatusSurface />
        <SessionSummary summary={state.sessionUsage} active={protectedNow || Boolean(state.sessionActive || state.reconnecting)} />
      </div>
      <div className="grid grid-cols-2 gap-4 @max-[600px]/overview:grid-cols-1">
        <div className="flex min-w-0 flex-col gap-4">
          <OverviewModule title="Local API" description="Use private AI in your tools." titleAdornment={<IconButton variant="ghost" size="icon-xs" label="Local API examples" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "local-api-example" })}><CircleHelp aria-hidden="true" /></IconButton>} status={<StateLabel tone={localAvailable ? "success" : "neutral"} text={localAvailable ? "Available" : "Unavailable"} />} action={<IconButton label="Local API settings" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "local-api" })}><Settings /></IconButton>}>
            <LocalApiPanel
              proxyUrl={state.proxyUrl}
              clientKey={shell.clientKey}
              clientKeyVisible={shell.clientKeyVisible}
              onToggleKey={shell.toggleClientKey}
            />
          </OverviewModule>
          <OverviewModule title="Agents" description="Use private AI in your agents." action={agents.accessStatus !== "authorized" || agents.authorizing
            ? <Button type="button" variant="outline" size="sm" disabled={!agents.accessStatus || agents.authorizing} aria-busy={agents.authorizing} onClick={agents.requestAccess}>
                {agents.authorizing && <LoaderCircle className="animate-spin" aria-hidden="true" />}Enable
              </Button>
            : <Link to="/agents" className={cn(buttonVariants({ variant: "outline", size: "sm" }))}>View All</Link>}>
            <ItemGroup>
              {agents.accessStatus === "authorized" && !agents.agents.some((agent) => agent.installed) ? <EmptyState text={agents.problem ? "Agent detection unavailable" : "No agents detected"} />
                : previewAgents.map((agent) => (
                <AgentRow
                  key={agent.id}
                  agent={agent}
                  compact
                  detectionLabel={agentDetectionLabel}
                  disabled={shell.applying || agents.controlsLocked || Boolean(agentDetectionLabel)}
                />
              ))}
            </ItemGroup>
          </OverviewModule>
        </div>
        <OverviewModule
          title="Recent usage"
          description="Latest requests in this session."
          action={<Link to="/usage" className={cn(buttonVariants({ variant: "outline", size: "sm" }))}>View All</Link>}
        >
          {/* Sized by the cards beside it, not by the list; one column gives it a height of its own. */}
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain contain-size @max-[600px]/overview:h-80 @max-[600px]/overview:flex-none" role="region" tabIndex={0} aria-label="Recent requests">
            {recent.length === 0
              ? <EmptyState text={state.protection.action.operation === "stop" || state.sessionActive ? "No requests in this session yet." : "Start protection to begin a new session."} />
              : <ItemGroup>{recent.map((item) => <UsageRow key={item.id} activity={item} onOpen={() => shell.openDialog({ kind: "usage-proof", activity: item })} />)}</ItemGroup>}
          </div>
        </OverviewModule>
      </div>
    </div>
  );
}

function StatusSurface(): React.JSX.Element {
  const shell = useShell();
  const { state } = shell;
  const protectedNow = state.protection.phase === "protected";
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <Card size="sm" role="region" className="relative isolate" aria-label="Protection status">
      <TrackLayer active={protectedNow} />
      <CardHeader>
        <CardTitle><ProtectionStatus state={state} /></CardTitle>
        <CardAction><ProtectedControl state={state} pending={shell.protectionPending} onToggle={shell.toggleProtection} /></CardAction>
      </CardHeader>
      <CardContent className="mt-auto flex min-w-0 items-center gap-2">
        {state.backendConnected === false ? <Button variant="outline" size="sm" disabled={shell.startingBackend} onClick={shell.startBackend}><RefreshCw className={shell.startingBackend ? "animate-spin" : undefined} aria-hidden="true" />{shell.startingBackend ? "Starting…" : "Start Background Service"}</Button> : <>
        <Button variant="outline" size="sm" className="max-w-40" aria-label={activeProfile ? `Profiles: ${activeProfile.name}` : "Set Up Profile"} aria-haspopup="dialog" onClick={shell.openProfiles}>
          {activeProfile ? <ServiceLogo provider={activeProfile.provider} /> : <Plus aria-hidden="true" />}
          <span className="truncate">{activeProfile?.name ?? "Set Up"}</span>
          {activeProfile && <ChevronDown aria-hidden="true" />}
        </Button>
        {activeProfile?.auth.kind === "oauth" && <AccountBalanceValue api={desktopApi} provider={activeProfile.provider} target={{ kind: "profile", profileId: activeProfile.id }} credentialRef={activeProfile.credentialRef} enabled={protectedNow} />}
        <IconButton size="icon-sm" label="Privacy verification" aria-haspopup="dialog" onClick={() => shell.openDialog({ kind: "privacy" })}><Info aria-hidden="true" /></IconButton>
        </>}
      </CardContent>
    </Card>
  );
}

/** Encrypted records scrolling behind the status while protection is on. */
const TrackLayer = memo(function TrackLayer({ active }: { active: boolean }): React.JSX.Element {
  return (
    <div className={cn("pointer-events-none absolute inset-0 -z-1 grid grid-rows-11 items-center overflow-hidden py-2 font-mono text-xs text-muted-foreground/5 transition-opacity duration-500 [mask-image:linear-gradient(to_right,transparent,#000_12%,#000_88%,transparent)] motion-reduce:transition-none", active ? "opacity-100" : "opacity-0")} aria-hidden="true">
      {TLS_TRACKS.map((line, index) => (
        <div key={line} className="overflow-hidden whitespace-nowrap">
          <div className={cn("flex w-max motion-reduce:animate-none", index % 2 === 1 ? "animate-track-right" : "animate-track-left", !active && "[animation-play-state:paused]")}>
            <span className="pr-8">{line}</span><span className="pr-8">{line}</span>
          </div>
        </div>
      ))}
    </div>
  );
});

function OverviewModule({
  title,
  description,
  titleAdornment,
  status,
  action,
  children,
}: React.PropsWithChildren<{
  title: string;
  description?: string;
  titleAdornment?: React.ReactNode;
  status?: React.ReactNode;
  action?: React.ReactNode;
}>): React.JSX.Element {
  return (
    <Card size="sm" className="min-w-0">
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2"><h2>{title}</h2>{titleAdornment}{status}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      <CardContent className="@container flex min-h-0 flex-1 flex-col">{children}</CardContent>
    </Card>
  );
}

function SessionSummary({ summary, active }: { summary: UsageSummary; active: boolean }): React.JSX.Element {
  const totalTokens = summary.inputTokens + summary.outputTokens;
  return (
    <Card size="sm" role="region" className="min-w-0" aria-labelledby="session-usage-heading">
      <CardHeader><CardTitle><h2 id="session-usage-heading">Current session</h2></CardTitle></CardHeader>
      <CardContent className="mt-auto flex min-w-0 gap-3" role="group" aria-label="Usage in this session">
        {[
          ["Requests", active ? summary.requests.toLocaleString() : "—"],
          ["Tokens", active ? formatTokens(totalTokens) : "—"],
          ["Estimated cost", active ? currency(summary.costUsd) : "—"],
        ].map(([label, value], index) => <React.Fragment key={label}>
          {index > 0 && <Separator orientation="vertical" />}
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            <span className="text-xs text-muted-foreground">{label}</span>
            <strong className="truncate text-xl font-semibold tabular-nums">{value}</strong>
          </div>
        </React.Fragment>)}
      </CardContent>
    </Card>
  );
}
