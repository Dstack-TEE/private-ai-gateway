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
  /** A settings change is applying; controls that change settings wait. */
  applying: boolean;
  startingBackend: boolean;
  startBackend(): void;
  /** Protection is starting or stopping at the window's request. */
  protectionPending: boolean;
  toggleProtection(): void;
  /** Asks first when protection must stop; its row runs it as `OS_POLICY_CHANGE`. */
  changeRequireProductionOs(required: boolean): Promise<void>;
  resetSettings(): void;
  /** A dialog request never replaces an open dialog, only one that is animating out. */
  openDialog(dialog: AppDialog): void;
  /** Profiles, or a new profile when there is none. */
  openProfiles(): void;
  openAboutLink(target: AboutLink): void;
}

/** The mutation key of an OS policy change, which makes the window `applying`. */
export const OS_POLICY_CHANGE = ["os-policy-change"];

export const ShellContext = createContext<Shell | null>(null);

export function useShell(): Shell {
  const shell = useContext(ShellContext);
  if (!shell) throw new Error("Pages render inside AppLayout");
  return shell;
}
