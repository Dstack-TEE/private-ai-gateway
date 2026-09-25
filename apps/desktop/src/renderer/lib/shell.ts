import { createContext, useContext } from "react";
import type { AboutLink, AppState, RequestActivity } from "../../shared/contracts";
import type { useAgents } from "../hooks/use-agents";
import type { useUpdates } from "../updates";

export type AppDialog =
  | { kind: "profiles"; repair: boolean }
  | { kind: "setup-profile" | "privacy" | "local-api" | "local-api-example" | "notifications" | "web-ui" }
  | { kind: "usage-proof"; activity: RequestActivity };

/** What the window around the pages (`AppLayout`) shares with them. */
export interface Shell {
  state: AppState;
  agents: ReturnType<typeof useAgents>;
  updates: ReturnType<typeof useUpdates>;
  clientKey: string;
  clientKeyVisible: boolean;
  toggleClientKey(): void;
  /** The label of the value copied last, while it shows as copied. */
  copied?: string;
  /** Copies a value; rejects when it was not copied. */
  copyValue(label: string, value: string): Promise<void>;
  /** Copies a value and reports a failure. */
  copy(label: string, value: string): void;
  /** A settings change is applying; controls that change settings wait. */
  applying: boolean;
  startingBackend: boolean;
  startBackend(): void;
  toggleProtection(): void;
  setRequireProductionOs(required: boolean): void;
  resetSettings(): void;
  /** A dialog request never replaces a dialog that is already open. */
  openDialog(dialog: AppDialog): void;
  /** Profiles, or a new profile when there is none. */
  openProfiles(): void;
  openAboutLink(target: AboutLink): void;
}

export const ShellContext = createContext<Shell | null>(null);

export function useShell(): Shell {
  const shell = useContext(ShellContext);
  if (!shell) throw new Error("Pages render inside AppLayout");
  return shell;
}
