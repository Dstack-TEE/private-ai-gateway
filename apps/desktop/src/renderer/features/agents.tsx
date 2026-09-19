import React from "react";
import { useErrorAlert } from "../lib/error-alert";
import { ExternalLink } from "lucide-react";
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
import { desktopApi } from "../lib/environment";
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
  return (
    <div className="page-body max-w-230 min-h-full mt-0 mr-auto mb-0 ml-auto">
      <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby="agents-title">
        <h2 className="group-title mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id="agents-title">{accessStatus === "authorized" ? "Installed" : "Agent Integrations"} <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{accessStatus === "authorized" ? `${connected} connected` : ""}</span></h2>
        <div className="inset min-w-0 bg-card border border-border rounded-2xl overflow-hidden">
          {accessStatus !== "authorized" ? <AgentAccessPrompt status={accessStatus} busy={authorizing} onAuthorize={onAuthorize} />
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

export function AgentAccessPrompt({ status, busy, onAuthorize }: {
  status?: AgentAccessStatus;
  busy: boolean;
  onAuthorize(): void;
}): React.JSX.Element {
  if (!status) return <EmptyState text="Checking Agent access…" />;
  const again = status === "reauthorizationRequired";
  return <div className="grid min-h-28 place-items-center gap-2 p-4 text-center">
    <p className="max-w-92 text-xs text-muted-foreground">Allow access to Agent configuration folders in Home to detect installed agents and configure those you choose to connect, using revocable local proxy tokens. Workspace contents are not read.</p>
    <Button type="button" variant="outline" size="sm" disabled={busy} onClick={onAuthorize}>
      {busy ? "Waiting for Home folder…" : again ? "Re-enable Agent Integrations" : "Enable Agent Integrations"}
    </Button>
  </div>;
}

export function AgentRow({
  pendingConnection,
  agent,
  disabled,
  compact = false,
  onSelect,
}: {
  pendingConnection?: boolean;
  agent: AgentStatus;
  disabled: boolean;
  compact?: boolean;
  onSelect(connect: boolean): void;
}): React.JSX.Element {
  const name = agent.name;
  const presence: { label: string; tone: Tone } = pendingConnection !== undefined
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
          {note && pendingConnection === undefined
            ? <AgentAttention name={name} message={note} authorized={agent.authorized} action={!disabled ? agent.repairAction : undefined} onRepair={() => onSelect(agent.repairAction === "reconnect")} />
            : <StateLabel tone={presence.tone} text={presence.label} />}
        </ItemTitle>
      </ItemContent>
      <ItemActions>
      {agent.installed ? <SwitchControl
        checked={disconnecting}
        aria-busy={pendingConnection !== undefined}
        disabled={disabled || !actionable}
        label={`${disconnecting ? "Disconnect" : "Connect"} ${name}`}
        onToggle={() => { if (!disabled && actionable) onSelect(!disconnecting); }}
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
