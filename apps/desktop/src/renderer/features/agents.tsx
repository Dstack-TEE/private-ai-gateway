import React from "react";
import { toastError } from "../lib/error-message";
import { ExternalLink, FolderLock, LoaderCircle, TriangleAlert } from "lucide-react";
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
import type { Tone } from "../../shared/contracts";
import { Item, ItemActions, ItemContent, ItemTitle } from "../components/ui/item";
import { SettingsSection } from "../components/settings";
import { SwitchControl } from "../components/controls";
import type { AgentAccessStatus, AgentStatus } from "../../shared/contracts";
import { EmptyState } from "../components/detail";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "../components/ui/alert";
import { desktopApi } from "../lib/environment";
import { useShell } from "../lib/shell";
import { useAgentConnection } from "../hooks/use-agents";
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

export function AgentsPage(): React.JSX.Element {
  const { agents: integrations, applying } = useShell();
  const { agents, accessStatus, authorizing, problem } = integrations;
  const connected = agents.filter((agent) => agent.installed && agent.connected).length;
  const detectionLabel = accessStatus === "authorized"
    ? undefined
    : authorizing
      ? "Waiting for access"
      : accessStatus
        ? "Access required"
        : "Checking access";
  const locked = applying || integrations.controlsLocked;
  return (
    <div className="mx-auto flex max-w-230 flex-col gap-5">
      <AgentAccessNotice status={accessStatus} busy={authorizing} onAuthorize={integrations.requestAccess} />
      {accessStatus === "authorized" && problem && <AgentDetectionNotice busy={authorizing} onRetry={integrations.requestAccess} />}
      <SettingsSection title={accessStatus === "authorized" && !problem ? "Detected" : "Agents"} detail={accessStatus === "authorized" && !problem ? `${connected} connected` : undefined}>
        {accessStatus !== "authorized" ? agents.map((agent) => (
          <AgentRow key={agent.id} agent={agent} detectionLabel={detectionLabel} disabled />
        ))
          : problem ? agents.map((agent) => (
            <AgentRow key={agent.id} agent={agent} detectionLabel="Detection unavailable" disabled />
          ))
          : !agents.some((agent) => agent.installed) ? <EmptyState text="No agents detected" />
          : agents.filter((agent) => agent.installed).map((agent) => (
            <AgentRow key={agent.id} agent={agent} disabled={locked} />
          ))}
      </SettingsSection>
      {accessStatus === "authorized" && !problem && agents.some((agent) => !agent.installed) && <SettingsSection title="Not detected">
        {agents.filter((agent) => !agent.installed).map((agent) => (
          <AgentRow key={agent.id} agent={agent} disabled={locked} />
        ))}
      </SettingsSection>}
      {accessStatus === "authorized" && !problem && <p className="text-xs text-muted-foreground">
        Agents are detected by the configuration folder each one creates on first run, such as ~/.codex, however it was installed. Run a new agent once to list it here; an uninstalled agent stays listed while its folder remains.
      </p>}
    </div>
  );
}

function AgentDetectionNotice({ busy, onRetry }: { busy: boolean; onRetry(): void }): React.JSX.Element {
  return <Alert role="status">
    <TriangleAlert aria-hidden="true" />
    <AlertTitle>Agent detection unavailable</AlertTitle>
    <AlertDescription>The background service could not use Home access.</AlertDescription>
    <AlertAction>
      <Button type="button" variant="outline" size="xs" disabled={busy} aria-busy={busy} onClick={onRetry}>
        {busy ? <><LoaderCircle className="animate-spin" aria-hidden="true" />Retrying…</> : "Retry"}
      </Button>
    </AlertAction>
  </Alert>;
}

function AgentAccessNotice({ status, busy, onAuthorize }: {
  status?: AgentAccessStatus;
  busy: boolean;
  onAuthorize(): void;
}): React.JSX.Element | null {
  if (status === "authorized") return null;
  const again = status === "reauthorizationRequired";
  return <Alert role="status">
    <FolderLock aria-hidden="true" />
    <AlertTitle>Home access required</AlertTitle>
    <AlertDescription>Allow it to detect installed agents and manage the connections you choose. Workspace files are not read.</AlertDescription>
    <AlertAction>
      <Button type="button" variant="outline" size="xs" disabled={!status || busy} aria-busy={busy} onClick={onAuthorize}>
        {busy ? <><LoaderCircle className="animate-spin" aria-hidden="true" />Waiting…</> : again ? "Re-enable" : "Enable"}
      </Button>
    </AlertAction>
  </Alert>;
}

export function AgentRow({
  agent,
  disabled,
  detectionLabel,
  compact = false,
}: {
  agent: AgentStatus;
  disabled: boolean;
  detectionLabel?: string;
  compact?: boolean;
}): React.JSX.Element {
  const { pending: pendingConnection, change: onSelect } = useAgentConnection(desktopApi, agent);
  const name = agent.name;
  const presence: { label: string; tone: Tone } = detectionLabel
    ? { label: detectionLabel, tone: "neutral" }
    : pendingConnection !== undefined
    ? { label: pendingConnection ? "Connecting…" : "Disconnecting…", tone: "neutral" }
    : !agent.installed
    ? { label: "Not detected", tone: "neutral" }
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
    <Item size={compact ? "xs" : "default"} variant={compact ? "muted" : "outline"}>
      <AgentMark agent={agent} />
      <ItemContent className="min-w-0">
        <ItemTitle>
          {name}
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
    </Item>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  return <Button variant="outline" onClick={() => {
    void desktopApi.openAgentWebsite(agent.id).catch((error: unknown) => toastError("Could not open agent website", error));
  }}>Website<ExternalLink data-icon="inline-end" aria-hidden="true" /></Button>;
}
