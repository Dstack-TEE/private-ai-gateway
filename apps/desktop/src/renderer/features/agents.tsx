import { displayAgentName, sortAgents } from "../lib/agents";
import React, { useState } from "react";
import { errorMessage } from "../lib/error-message";
import { ExternalLink, TriangleAlert } from "lucide-react";
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
import { type Tone } from "../lib/usage-presentation";
import { Alert, AlertDescription } from "../components/ui/alert";
import { Item, ItemActions, ItemContent, ItemTitle } from "../components/ui/item";
import { Separator } from "../components/ui/separator";
import { SwitchControl } from "../components/controls";
import type { AgentStatus } from "../../shared/contracts";
import { EmptyState } from "../components/detail";
import { desktopApi } from "../lib/environment";

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
    <span className={agent.id === "oh-my-pi" ? "mark [&.mark-oh-my-pi]:bg-[#0d0d0d] flex-none w-8 h-8 grid place-items-center overflow-hidden text-muted-foreground bg-white border border-border rounded-xl text-xs font-bold [&_img]:w-5 [&_img]:h-5 [&_img]:object-contain mark-oh-my-pi" : "mark [&.mark-oh-my-pi]:bg-[#0d0d0d] flex-none w-8 h-8 grid place-items-center overflow-hidden text-muted-foreground bg-white border border-border rounded-xl text-xs font-bold [&_img]:w-5 [&_img]:h-5 [&_img]:object-contain"} aria-hidden="true">
      {icon ? <img src={icon} alt="" /> : agent.name.slice(0, 2).toUpperCase()}
    </span>
  );
}

export function AgentsView({
  pendingAgentChanges,
  agents,
  locked,
  problem,
  onSelect,
}: {
  pendingAgentChanges: Record<string, boolean>;
  agents: AgentStatus[];
  locked: boolean;
  problem?: string;
  onSelect(agent: AgentStatus, connect: boolean): void;
}): React.JSX.Element {
  const connected = agents.filter((agent) => agent.installed && agent.connected).length;
  return (
    <div className="page-body max-w-230 min-h-full mt-0 mr-auto mb-0 ml-auto">
      {problem && <Alert variant="destructive"><AlertDescription>{problem}</AlertDescription></Alert>}

      <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby="agents-title">
        <h2 className="group-title mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id="agents-title">Installed <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{connected} connected</span></h2>
        <div className="inset min-w-0 bg-card border border-border rounded-2xl overflow-hidden">
          {!agents.some((agent) => agent.installed) && <EmptyState text="No installed agents found" />}
          {sortAgents(agents.filter((agent) => agent.installed)).map((agent) => (
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
      {agents.some((agent) => !agent.installed) && <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby="not-installed-title">
        <h2 className="group-title mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id="not-installed-title">Not installed</h2>
        <div className="inset min-w-0 bg-card border border-border rounded-2xl overflow-hidden">{sortAgents(agents.filter((agent) => !agent.installed)).map((agent) => (
          <AgentRow key={agent.id} agent={agent} disabled={locked} onSelect={() => undefined} />
        ))}</div>
      </section>}
    </div>
  );
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
  const name = displayAgentName(agent);
  const presence = pendingConnection !== undefined
    ? { label: pendingConnection ? "Connecting…" : "Disconnecting…", tone: "neutral" as Tone, icon: undefined }
    : !agent.installed
    ? { label: "Not installed", tone: "neutral" as Tone, icon: undefined }
    : agent.attention
      ? { label: "Needs attention", tone: "warning" as Tone, icon: TriangleAlert }
      : agent.error
        ? { label: "Error", tone: "danger" as Tone, icon: TriangleAlert }
        : agent.connected
          ? { label: "Connected", tone: "success" as Tone, icon: undefined }
          : { label: "Not connected", tone: "neutral" as Tone, icon: undefined };
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
        onToggle={() => onSelect(!disconnecting)}
      /> : <AgentWebsite agent={agent} />}
      </ItemActions>
    </Item>{!compact && <Separator className="last:hidden" />}</>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  const [error, setError] = useState<string>();
  return <span><Button variant="outline" onClick={() => {
    setError(undefined);
    void desktopApi.openAgentWebsite(agent.id).catch((error: unknown) => setError(errorMessage(error)));
  }}>Website<ExternalLink size={14} aria-hidden="true" /></Button>{error && <span className="row-note flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap" role="alert">{error}</span>}</span>;
}
