import type { AgentStatus } from "../../shared/contracts";

export function displayAgentName(agent: Pick<AgentStatus, "id" | "name">): string {
  return agent.id === "hermes" ? "Hermes Agent" : agent.name;
}

export function sortAgents(agents: AgentStatus[]): AgentStatus[] {
  const order = ["claude-code", "codex", "hermes", "pi", "oh-my-pi", "opencode", "openclaw"];
  return [...agents].sort((left, right) => {
    const leftIndex = order.indexOf(left.id);
    const rightIndex = order.indexOf(right.id);
    return (leftIndex < 0 ? order.length : leftIndex) - (rightIndex < 0 ? order.length : rightIndex);
  });
}
