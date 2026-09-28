import React from "react";
import { ExternalLink, FolderLock, LoaderCircle } from "lucide-react";
import claudeCodeIcon from "@lobehub/icons-static-svg/icons/claudecode-color.svg";
import codexIcon from "@lobehub/icons-static-svg/icons/codex-color.svg";
import hermesIcon from "@lobehub/icons-static-svg/icons/hermesagent.svg";
import kiloIcon from "@lobehub/icons-static-svg/icons/kilocode.svg";
import openCodeIcon from "@lobehub/icons-static-svg/icons/opencode.svg";
import openClawIcon from "@lobehub/icons-static-svg/icons/openclaw-color.svg";
import piIcon from "@lobehub/icons-static-svg/icons/pi.svg";
import qwenCodeIcon from "@lobehub/icons-static-svg/icons/qwen-color.svg";
import { Button } from "../components/ui/button";
import { StateLabel } from "../components/state-label";
import { AgentAttention } from "../components/agent-attention";
import ohMyPiIcon from "../assets/oh-my-pi.svg";
import type { Tone } from "../../shared/contracts";
import { Item, ItemActions, ItemContent, ItemTitle } from "../components/ui/item";
import { SettingsItem, SettingsSection } from "../components/settings";
import { SwitchControl } from "../components/controls";
import type { AgentAccessStatus, AgentStatus } from "../../shared/contracts";
import { EmptyState } from "../components/detail";
import { Alert, AlertDescription } from "../components/ui/alert";
import { desktopApi } from "../lib/environment";
import { useShell } from "../lib/shell";
import { useAgentConnection } from "../hooks/use-agents";
import { useReportFailure } from "../components/confirm";
import { cn } from "../lib/utils";

const AGENT_ICONS: Record<string, string> = {
  codex: codexIcon,
  "claude-code": claudeCodeIcon,
  opencode: openCodeIcon,
  pi: piIcon,
  "oh-my-pi": ohMyPiIcon,
  hermes: hermesIcon,
  openclaw: openClawIcon,
  "qwen-code": qwenCodeIcon,
  kilo: kiloIcon,
};

function AgentMark({ agent }: { agent: Pick<AgentStatus, "id" | "name"> }): React.JSX.Element {
  const icon = AGENT_ICONS[agent.id];
  return (
    <span className={cn(
      "grid size-8 flex-none place-items-center overflow-hidden rounded-xl border border-border bg-white text-xs font-bold text-muted-foreground",
      agent.id === "oh-my-pi" && "bg-[#0d0d0d]",
    )} aria-hidden="true">
      {icon ? <img className="size-5 object-contain" src={icon} alt="" /> : agent.name.slice(0, 2).toUpperCase()}
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
    <div className="mx-auto min-h-full max-w-230">
      <AgentAccessNotice status={accessStatus} busy={authorizing} onAuthorize={integrations.requestAccess} />
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
      {accessStatus === "authorized" && !problem && <p className="mx-0.5 mt-3 text-xs leading-5 text-muted-foreground">
        Agents are detected by the configuration folder each one creates on first run, such as ~/.codex, however it was installed. Run a new agent once to list it here; an uninstalled agent stays listed while its folder remains.
      </p>}
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
  const { agents: { offerServiceStop } } = useShell();
  const { pending: pendingConnection, change: onSelect } = useAgentConnection(desktopApi, agent, offerServiceStop);
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
          // Saved, its own configuration restored until protection resumes.
          : agent.recorded
            ? { label: "Inactive", tone: "neutral" }
            : { label: "Not connected", tone: "neutral" };
  const disconnecting = pendingConnection ?? agent.recorded;
  const actionable = disconnecting || !agent.error;
  const note = agent.attention ?? agent.error;
  const Row = compact ? Item : SettingsItem;
  return (
    // The overview's last compact row drops its bottom border.
    <Row size={compact ? "xs" : "default"} variant={compact ? "muted" : "default"} className={cn(compact && "last:border-b-0")}>
      <AgentMark agent={agent} />
      <ItemContent className="min-w-0">
        <ItemTitle className="flex max-w-full flex-wrap items-center gap-x-2 gap-y-1">
          <span>{name}</span>
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
    </Row>
  );
}

function AgentWebsite({ agent }: { agent: AgentStatus }): React.JSX.Element {
  const reportFailure = useReportFailure();
  return <Button variant="outline" onClick={() => {
    void desktopApi.openAgentWebsite(agent.id).catch((error: unknown) => reportFailure("Could not open agent website", error));
  }}>Website<ExternalLink size={14} aria-hidden="true" /></Button>;
}
