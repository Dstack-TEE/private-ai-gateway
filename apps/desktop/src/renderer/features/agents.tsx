import React from "react";
import { useErrorAlert } from "../lib/error-alert";
import { ExternalLink, FolderLock, LoaderCircle } from "lucide-react";
import claudeCodeIcon from "@lobehub/icons-static-svg/icons/claudecode-color.svg";
import codexIcon from "@lobehub/icons-static-svg/icons/codex-color.svg";
import hermesIcon from "@lobehub/icons-static-svg/icons/hermesagent.svg";
import openCodeIcon from "@lobehub/icons-static-svg/icons/opencode.svg";
import openClawIcon from "@lobehub/icons-static-svg/icons/openclaw-color.svg";
import piIcon from "@lobehub/icons-static-svg/icons/pi.svg";
import { Button } from "../components/ui/button";
import { StateLabel } from "../components/state-label";
import { AgentAttention } from "../components/agent-attention";
import ohMyPiIcon from "../assets/oh-my-pi.svg";
import type { Tone } from "../lib/tone";
import { Item, ItemActions, ItemContent, ItemTitle } from "../components/ui/item";
import { Separator } from "../components/ui/separator";
import { SwitchControl } from "../components/controls";
import type { AgentAccessStatus, AgentStatus } from "../../shared/contracts";
import { EmptyState } from "../components/detail";
import { Alert, AlertDescription } from "../components/ui/alert";
import { desktopApi } from "../lib/environment";
import { supportedAgentStatuses } from "../lib/agent-integrations";
import { cn } from "../lib/utils";

const AGENT_ICONS: Record<string, string> = {
  codex: codexIcon,
  "claude-code": claudeCodeIcon,
  opencode: openCodeIcon,
  pi: piIcon,
  "oh-my-pi": ohMyPiIcon,
  hermes: hermesIcon,
  openclaw: openClawIcon,
};

function AgentMark({ agent }: { agent: Pick<AgentStatus, "id" | "name"> }): React.JSX.Element {
  const icon = AGENT_ICONS[agent.id];
  return (
    <span className={cn(
      "grid size-8 flex-none place-items-center overflow-hidden rounded-xl border border-border bg-white text-xs font-bold text-muted-foreground [&_img]:size-5 [&_img]:object-contain",
      agent.id === "oh-my-pi" && "bg-[#0d0d0d]",
    )} aria-hidden="true">
      {icon ? <img src={icon} alt="" /> : agent.name.slice(0, 2).toUpperCase()}
    </span>
  );
}

export function AgentsView({
  accessStatus,
  authorizing,
  pendingAgentChanges,
  agents,
  locked,
  problem,
  onSelect,
  onAuthorize,
}: {
  accessStatus?: AgentAccessStatus;
  authorizing: boolean;
  pendingAgentChanges: Record<string, boolean>;
  agents: AgentStatus[];
  locked: boolean;
  problem?: string;
  onSelect(agent: AgentStatus, connect: boolean): void;
  onAuthorize(): void;
}): React.JSX.Element {
  const connected = agents.filter((agent) => agent.installed && agent.connected).length;
  const listedAgents = agents.length > 0 ? agents : supportedAgentStatuses();
  const detectionLabel = accessStatus === "authorized"
    ? undefined
    : authorizing
      ? "Waiting for access"
      : accessStatus
        ? "Access required"
        : "Checking access";
  return (
    <div className="page-body max-w-230 min-h-full mt-0 mr-auto mb-0 ml-auto">
      <AgentAccessNotice status={accessStatus} busy={authorizing} onAuthorize={onAuthorize} />
      <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby="agents-title">
        <h2 className="group-title mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id="agents-title">{accessStatus === "authorized" ? "Installed" : "Agents"} <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{accessStatus === "authorized" ? `${connected} connected` : ""}</span></h2>
        <div className="inset min-w-0 bg-card border border-border rounded-2xl overflow-hidden">
          {accessStatus !== "authorized" ? listedAgents.map((agent) => (
            <AgentRow
              key={agent.id}
              agent={agent}
              detectionLabel={detectionLabel}
              disabled
              onSelect={() => undefined}
            />
          ))
            : !agents.some((agent) => agent.installed) ? <EmptyState text={problem ? "Agent detection unavailable" : "No installed agents found"} />
            : agents.filter((agent) => agent.installed).map((agent) => (
            <AgentRow
              pendingConnection={pendingAgentChanges[agent.id]}
              key={agent.id}
              agent={agent}
              disabled={locked}
              onSelect={(connect) => onSelect(agent, connect)}
            />
          ))}
        </div>
      </section>
      {accessStatus === "authorized" && agents.some((agent) => !agent.installed) && <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby="not-installed-title">
        <h2 className="group-title mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id="not-installed-title">Not installed</h2>
        <div className="inset min-w-0 bg-card border border-border rounded-2xl overflow-hidden">{agents.filter((agent) => !agent.installed).map((agent) => (
          <AgentRow key={agent.id} agent={agent} disabled={locked} onSelect={() => undefined} />
        ))}</div>
      </section>}
    </div>
  );
}

function AgentAccessNotice({ status, busy, onAuthorize }: {
  status?: AgentAccessStatus;
  busy: boolean;
  onAuthorize(): void;
}): React.JSX.Element {
  if (status === "authorized") return <></>;
  const again = status === "reauthorizationRequired";
  return <Alert role="status" className="mb-5 rounded-xl border-border bg-muted/35 px-3.5 py-2.5">
    <FolderLock size={16} aria-hidden="true" />
    <AlertDescription className="col-start-2 flex flex-wrap items-center justify-between gap-3 text-xs leading-5">
      <span className="min-w-0 flex-1"><strong className="font-medium text-foreground">Home access required.</strong> Allow it to detect installed agents and manage the connections you choose. Workspace files are not read.</span>
      <Button type="button" variant="outline" size="sm" className="shrink-0" disabled={!status || busy} aria-busy={busy} onClick={onAuthorize}>
        {busy ? <><LoaderCircle size={14} className="animate-spin" aria-hidden="true" />Waiting…</> : again ? "Re-enable" : "Enable"}
      </Button>
    </AlertDescription>
  </Alert>;
}

export function AgentRow({
  pendingConnection,
  agent,
  disabled,
  detectionLabel,
  compact = false,
  onSelect,
}: {
  pendingConnection?: boolean;
  agent: AgentStatus;
  disabled: boolean;
  detectionLabel?: string;
  compact?: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const name = agent.name;
  const presence: { label: string; tone: Tone } = detectionLabel
    ? { label: detectionLabel, tone: "neutral" }
    : pendingConnection !== undefined
    ? { label: pendingConnection ? "Connecting…" : "Disconnecting…", tone: "neutral" }
    : !agent.installed
    ? { label: "Not installed", tone: "neutral" }
    : agent.attention
      ? { label: "Needs attention", tone: "warning" }
      : agent.error
        ? { label: "Error", tone: "danger" }
        : agent.connected
          ? { label: "Connected", tone: "success" }
          : { label: "Not connected", tone: "neutral" };
  const disconnecting = pendingConnection ?? agent.recorded;
  const actionable = disconnecting || !agent.error;
  const note = agent.attention ?? agent.error;
  return (
    <><Item size={compact ? "xs" : "default"} variant={compact ? "muted" : "default"} className="agent-block">
      <AgentMark agent={agent} />
      <ItemContent className="min-w-0">
        <ItemTitle className="row-title-line max-w-full flex items-center flex-wrap gap-y-1 gap-x-2">
          <span className="row-title">{name}</span>
          {note && pendingConnection === undefined && !detectionLabel
            ? <AgentAttention name={name} message={note} authorized={agent.authorized} action={!disabled ? agent.repairAction : undefined} onRepair={() => onSelect(agent.repairAction === "reconnect")} />
            : <StateLabel tone={presence.tone} text={presence.label} />}
        </ItemTitle>
      </ItemContent>
      <ItemActions>
      {agent.installed || detectionLabel ? <SwitchControl
        checked={disconnecting}
        aria-busy={pendingConnection !== undefined}
        disabled={disabled || Boolean(detectionLabel) || !actionable}
        label={`${disconnecting ? "Disconnect" : "Connect"} ${name}`}
        onToggle={() => { if (!disabled && !detectionLabel && actionable) onSelect(!disconnecting); }}
      /> : <AgentWebsite agent={agent} />}
      </ItemActions>
    </Item>{!compact && <Separator className="last:hidden" />}</>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  const reportError = useErrorAlert("Could not open agent website");
  return <Button variant="outline" onClick={() => {
    void desktopApi.openAgentWebsite(agent.id).catch(reportError);
  }}>Website<ExternalLink size={14} aria-hidden="true" /></Button>;
}
