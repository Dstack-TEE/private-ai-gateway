import type { GatewayState, GatewayStatus } from "../shared/contracts";

export interface TrayActions {
  copyEndpoint(endpoint: string): void;
  openWindow(): void;
  quit(): void;
  start(): void;
  stop(): void;
}

export interface TrayMenuItem {
  label?: string;
  type?: "normal" | "separator";
  enabled?: boolean;
  click?: () => void;
}

export function buildTrayMenu(
  state: GatewayState,
  actions: TrayActions,
): TrayMenuItem[] {
  const running = state.status === "verified" || state.status === "blocked";
  const busy = state.status === "verifying";
  const proxyUrl = state.proxyUrl;

  return [
    { label: "Private AI Gateway", enabled: false },
    { label: `Status: ${statusLabel(state.status)}`, enabled: false },
    { type: "separator" },
    { label: "Open Private AI Gateway", click: actions.openWindow },
    running
      ? { label: "Stop Gateway", click: actions.stop }
      : { label: busy ? "Verifying..." : "Start Gateway", enabled: !busy, click: actions.start },
    { type: "separator" },
    {
      label: "Copy OpenAI Endpoint",
      enabled: proxyUrl !== undefined,
      click: proxyUrl ? () => actions.copyEndpoint(proxyUrl) : undefined,
    },
    {
      label: "Copy Anthropic Endpoint",
      enabled: proxyUrl !== undefined,
      click: proxyUrl ? () => actions.copyEndpoint(proxyUrl) : undefined,
    },
    { type: "separator" },
    { label: "Quit Private AI Gateway", click: actions.quit },
  ];
}

export function statusLabel(status: GatewayStatus): string {
  switch (status) {
    case "stopped":
      return "Stopped";
    case "verifying":
      return "Verifying";
    case "verified":
      return "Verified";
    case "blocked":
      return "Blocked";
    case "error":
      return "Error";
  }
}
