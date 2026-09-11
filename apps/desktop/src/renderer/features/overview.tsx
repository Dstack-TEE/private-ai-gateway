import React, { memo } from "react";
import { AccountTools } from "../components/account-tools";
import { ChevronDown, CircleHelp, Info, Plus, Settings } from "lucide-react";
import { Button } from "../components/ui/button";
import { StateLabel } from "../components/state-label";
import { Hint } from "../components/hint";
import { currency, formatTokens } from "../lib/usage-presentation";
import { Badge } from "../components/ui/badge";
import { Card, CardHeader, CardTitle, CardDescription, CardAction, CardContent } from "../components/ui/card";
import { Separator } from "../components/ui/separator";
import { IconButton } from "../components/controls";
import type { AgentStatus, GatewayState, RequestActivity, UsageSummary } from "../../shared/contracts";
import { isProtected, presentation, profileHasCredential } from "../lib/protection";
import { LocalApiPanel } from "./local-api";
import { EmptyState } from "../components/detail";
import { AgentRow } from "./agents";
import { sortAgents } from "../lib/agents";
import { UsageRow } from "./usage";
import { ProtectedControl, ProtectionStatus } from "../components/protection";
import { ServiceLogo } from "../components/brand";
import { desktopApi } from "../lib/environment";

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

export function Overview({
  pendingAgentChanges,
  state,
  agents,
  busy,
  running,
  endpointDown,
  developmentMode,
  problem,
  locked,
  clientKey,
  clientKeyVisible,
  copied,
  onToggle,
  onSettings,
  onPrivacy,
  onLocalSettings,
  onLocalExamples,
  onAgents,
  onUsage,
  onCopy,
  onToggleClientKey,
  onSelect,
  onInspect,
}: {
  pendingAgentChanges: Record<string, boolean>;
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  problem?: string;
  locked: boolean;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  onToggle(): void;
  onSettings(): void;
  onPrivacy(): void;
  onLocalSettings(): void;
  onLocalExamples(): void;
  onAgents(): void;
  onUsage(): void;
  onCopy(label: string, value: string): Promise<void>;
  onToggleClientKey(): void;
  onSelect(agent: AgentStatus, connect: boolean): void;
  onInspect(activity: RequestActivity): void;
}): React.JSX.Element {
  const protectedNow = isProtected(state);
  const localAvailable = isProtected(state) && Boolean(state.proxyUrl) && !state.endpointError;
  const recent = protectedNow || state.sessionActive || state.reconnecting ? state.activity.slice(0, 10) : [];
  return (
    <div className="overview-page max-w-240 min-h-full mt-0 mr-auto mb-0 ml-auto flex flex-col @container/overview @max-[600px]/overview:[&_.overview-grid_>_.overview-module:nth-child(n)]:col-auto @max-[600px]/overview:[&_.overview-grid_>_.overview-module:nth-child(n)]:row-auto">
      <div className="overview-top grid *:min-h-36 grid-cols-2 gap-4 items-stretch [&_.status-surface.status-compact]:min-w-0 @max-[600px]/overview:grid-cols-1">
      <StatusSurface
        state={state}
        agents={agents}
        busy={busy}
        running={running}
        endpointDown={endpointDown}
        developmentMode={developmentMode}
        problem={problem}
        onToggle={onToggle}
        onSettings={onSettings}
        onPrivacy={onPrivacy}
      />
      <SessionSummary summary={state.sessionUsage} active={protectedNow || Boolean(state.sessionActive || state.reconnecting)} />
      </div>
      <div className="overview-grid mt-4 grid grid-cols-2 grid-rows-[auto_auto] gap-4 @max-[540px]/overview:grid-cols-1 [&_>_.overview-module:first-child]:col-start-1 [&_>_.overview-module:first-child]:row-start-1 [&_>_.overview-module:nth-child(2)]:col-start-1 [&_>_.overview-module:nth-child(2)]:row-start-2 [&_>_.overview-module:nth-child(3)]:col-start-2 [&_>_.overview-module:nth-child(3)]:row-[1_/_span_2] max-[780px]:grid-cols-1 max-[440px]:gap-3">
        <OverviewModule title="Local API" titleAdornment={<Hint content="Local API examples"><Badge variant="ghost" className="size-6 p-0 [&>svg]:size-4!" render={<button type="button" />} aria-label="Local API examples" aria-haspopup="dialog" onClick={onLocalExamples}><CircleHelp aria-hidden="true" /></Badge></Hint>} status={<StateLabel tone={localAvailable ? "success" : "neutral"} text={localAvailable ? "Available" : "Unavailable"} />} action={<IconButton label="Local API settings" aria-haspopup="dialog" onClick={onLocalSettings}><Settings size={16} /></IconButton>}>
          <LocalApiPanel
            proxyUrl={state.proxyUrl}
            endpointError={state.endpointError}
            clientKey={clientKey}
            clientKeyVisible={clientKeyVisible}
            copied={copied}
            onCopy={onCopy}
            onToggleKey={onToggleClientKey}
          />
        </OverviewModule>
        <OverviewModule title="Agents" description="Use private AI in your agents." action="View all" onAction={onAgents}>
          <div className="preview-list [&_>_:last-child]:border-b-0 overview-agent-list [--agent-row-height:calc(2rem_+_1.25rem_+_2px)] grid grid-rows-[repeat(3,_minmax(var(--agent-row-height),_auto))] gap-3 [&_>_.empty-state]:row-span-full">
            {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
            {sortAgents(agents.filter((agent) => agent.installed)).slice(0, 3).map((agent) => (
              <AgentRow
                pendingConnection={pendingAgentChanges[agent.id]}
                key={agent.id}
                agent={agent}
                compact
                disabled={locked}
                onSelect={(connect) => onSelect(agent, connect)}
              />
            ))}
          </div>
        </OverviewModule>
        <OverviewModule
          title="Recent usage"
          description="Latest requests in this session."
          action="View all"
          onAction={onUsage}
        >
          <div className="preview-list max-h-80 min-h-0 overflow-y-auto overscroll-contain [&_>_:last-child]:border-b-0" role="region" tabIndex={0} aria-label="Recent requests">
            {recent.length === 0 && (
              <EmptyState text={running || state.sessionActive || state.reconnecting ? "No requests in this session yet." : "Start protection to begin a new session."} />
            )}
            {recent.map((item) => (
              <React.Fragment key={item.id}><UsageRow activity={item} onOpen={() => onInspect(item)} /><Separator className="last:hidden" /></React.Fragment>
            ))}
          </div>
        </OverviewModule>
      </div>
    </div>
  );
}

function StatusSurface({
  state,
  agents,
  busy,
  running,
  endpointDown,
  developmentMode,
  problem,
  onToggle,
  onSettings,
  onPrivacy,
}: {
  state: GatewayState;
  agents: AgentStatus[];
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  problem?: string;
  onToggle(): void;
  onSettings(): void;
  onPrivacy(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const protectedNow = isProtected(state);
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <Card size="sm" role="region" className={`status-surface relative isolate transition-colors duration-200 ease-out motion-reduce:transition-none status-compact [&_.protected-control]:col-start-2 [&_.protected-control]:row-start-1 [&_.protected-control]:self-start [&_.protected-control]:justify-self-end [&_.protected-control]:min-h-[calc(var(--text-2xl)_*_var(--text-2xl--line-height))] [&_.status-profile]:w-[min(140px,_100%)] [&_.status-profile]:bg-card [&_.is-icon-only]:m-0 [&_.protection-status]:justify-start [&_.status-heading]:transition-colors [&_.status-heading]:duration-200 [&_.status-heading]:ease-out [&_[data-slot=switch]]:transition-colors [&_[data-slot=switch]]:duration-200 [&_[data-slot=switch]]:ease-out motion-reduce:[&_.status-heading]:transition-none motion-reduce:[&_[data-slot=switch]]:transition-none status-${state.status} ${protectedNow ? developmentMode ? "status-ready ring-warning dark:ring-warning shadow-warning/10" : "status-ready ring-primary dark:ring-primary shadow-primary/10" : ""} ${developmentMode ? "is-development" : ""}`} aria-label="Protection status">
      <TrackLayer active={protectedNow} />
      <CardContent className="status-compact-content relative z-2 grid grid-cols-[minmax(0,_1fr)_44px] grid-rows-[auto_1fr] gap-y-2 gap-x-3 flex-1 w-full">
        <div className={`[&.state-success]:text-primary [&.state-neutral]:text-muted-foreground [&.state-warning]:text-warning [&.state-danger]:text-destructive status-heading col-start-1 row-start-1 min-w-0 [&_.protection-status]:grid [&_.protection-status]:grid-cols-[24px_minmax(0,_1fr)] [&_.protection-status]:gap-y-1 [&_.protection-status]:gap-x-1.5 [&_.protection-status]:items-center [&_.protection-status]:text-2xl [&_.protection-status]:font-semibold [&_.protection-status_>_svg]:w-6 [&_.protection-status_>_svg]:h-6 [&_.protection-duration]:col-start-2 [&_.protection-duration]:text-xs [&_.protection-duration]:font-normal state-${verdict.tone}`}>
          <ProtectionStatus state={state} label={verdict.title} problem={problem} />
        </div>
        <div className="status-profile-actions col-span-full row-start-2 self-end flex items-center gap-2 min-w-0">
        <Button id="overview-profile" variant="outline" size="sm" className="status-profile w-[min(128px,_100%)] min-w-0 [&_>_span:not(.service-logo):not(.service-custom-icon)]:min-w-0 [&_>_span:not(.service-logo):not(.service-custom-icon)]:flex-1 [&_>_span:not(.service-logo):not(.service-custom-icon)]:overflow-hidden [&_>_span:not(.service-logo):not(.service-custom-icon)]:text-left [&_>_span:not(.service-logo):not(.service-custom-icon)]:text-ellipsis [&_>_span:not(.service-logo):not(.service-custom-icon)]:whitespace-nowrap [&_>_svg]:flex-none [&_.service-logo]:w-5 [&_.service-logo]:h-5 [&_.service-custom-icon]:w-5 [&_.service-custom-icon]:h-5" aria-label={activeProfile ? `Profiles: ${activeProfile.name}` : "Set up profile"} aria-haspopup="dialog" onClick={onSettings}>
          {activeProfile ? <ServiceLogo url={activeProfile.remoteUrl} /> : <Plus aria-hidden="true" />}
          <span>{activeProfile?.name ?? "Set up"}</span>
          {activeProfile && <ChevronDown aria-hidden="true" />}
        </Button>
        {activeProfile?.auth.kind === "oauth" && profileHasCredential(activeProfile) && <AccountTools
          api={desktopApi} provider={activeProfile.provider} target={{ kind: "profile", profileId: activeProfile.id }}
          credentialRef={activeProfile.credentialRef} scope={activeProfile.auth.scope} compact />}
        <IconButton size="icon-sm" label="Privacy verification" aria-haspopup="dialog" onClick={onPrivacy}><Info aria-hidden="true" /></IconButton>
        </div>
        <ProtectedControl state={state} busy={busy} running={running} endpointDown={endpointDown} developmentMode={developmentMode} onToggle={onToggle} iconOnly />
      </CardContent>
    </Card>
  );
}

const TrackLayer = memo(function TrackLayer({ active }: { active: boolean }): React.JSX.Element {
  return (
    <div className={`track-layer tracks-right absolute inset-0 z-1 grid grid-rows-11 items-center overflow-hidden py-2 pointer-events-none text-[color-mix(in_srgb,_var(--muted-foreground)_5%,_var(--card))] [mask-image:linear-gradient(to_right,_transparent,_#000_12%,_#000_88%,_transparent)] transition-opacity duration-500 motion-reduce:transition-none ${active ? "opacity-100 [&_.track-strip]:[animation-play-state:running]" : "opacity-0"}`} aria-hidden="true">
      {TLS_TRACKS.map((line, index) => <TrackRow key={line} text={line} reverse={index % 2 === 1} />)}
    </div>
  );
});

function TrackRow({ text, reverse }: { text: string; reverse: boolean }): React.JSX.Element {
  return (
    <div className={`track-row min-w-0 overflow-hidden flex items-center text-xs leading-4.5 font-mono whitespace-nowrap ${reverse ? "track-reverse [&_.track-strip]:animate-track-right" : ""}`}>
      <div className="track-strip w-[max-content] flex animate-track-left [animation-play-state:paused] motion-reduce:animate-none">
        <span className="track-copy flex-none pr-8">{text}</span><span className="track-copy flex-none pr-8">{text}</span>
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
  onAction,
  children,
}: React.PropsWithChildren<{
  title: string;
  description?: string;
  titleAdornment?: React.ReactNode;
  status?: React.ReactNode;
  action?: React.ReactNode;
  onAction?(): void;
}>): React.JSX.Element {
  return (
    <Card size="sm" className="overview-module min-w-0 [&_.agent-config]:hidden">
      <CardHeader className="items-center">
        <CardTitle className="overview-module-title flex items-center flex-wrap gap-2"><h2 className="text-base font-medium">{title}</h2>{titleAdornment}{status}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {action && <CardAction>{onAction ? <Button variant="outline" size="sm" onClick={onAction}>{action}</Button> : action}</CardAction>}
      </CardHeader>
      <CardContent className="module min-w-0 @container min-h-0 flex-1">{children}</CardContent>
    </Card>
  );
}

function SessionSummary({ summary, active }: { summary: UsageSummary; active: boolean }): React.JSX.Element {
  const totalTokens = summary.inputTokens + summary.outputTokens;
  return (
    <Card size="sm" role="region" className="session-overview min-w-0" aria-labelledby="session-usage-heading">
      <CardHeader><CardTitle><h2 id="session-usage-heading" className="text-base font-medium">Current session</h2></CardTitle></CardHeader>
    <CardContent className="session-summary mt-auto flex min-w-0 items-stretch gap-3" role="group" aria-label="Usage in this session">
      {[
        ["Requests", active ? summary.requests.toLocaleString() : "—"],
        ["Tokens", active ? formatTokens(totalTokens) : "—"],
        ["Estimated cost", active ? currency(summary.costUsd) : "—"],
      ].map(([label, value], index) => <React.Fragment key={label}>
        {index > 0 && <Separator orientation="vertical" className="h-auto self-stretch" />}
        <div data-slot="session-metric" className="flex min-w-0 flex-1 flex-col justify-between gap-2">
          <span className="text-xs text-muted-foreground">{label}</span>
          <strong className="truncate text-xl font-semibold tabular-nums">{value}</strong>
        </div>
      </React.Fragment>)}
    </CardContent>
    </Card>
  );
}
