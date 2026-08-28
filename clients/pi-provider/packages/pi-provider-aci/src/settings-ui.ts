// Settings UI helpers for the <provider-id>-settings command. Pure formatting
// and theme adapters; the interactive SettingsList wiring lives in index.ts.

import os from "node:os";
import { relative } from "node:path";
import type { Theme } from "@earendil-works/pi-coding-agent";
import type { SettingsListTheme } from "@earendil-works/pi-tui";

import {
  type AciCloudConfig,
  getGlobalAciCloudConfigPath,
  getProjectAciCloudConfigPath,
} from "./config.ts";
import { PROVIDER_VERSION } from "./constants.ts";

export type AciConfigScope = "project" | "home";

export const BOOLEAN_VALUES = ["true", "false"] as const;

export function buildSettingsTheme(theme: Theme): SettingsListTheme {
  return {
    label: (text: string, selected: boolean) => (selected ? theme.fg("accent", text) : text),
    value: (text: string, selected: boolean) =>
      selected ? theme.bold(theme.fg("accent", text)) : theme.fg("muted", text),
    description: (text: string) => theme.fg("dim", text),
    cursor: theme.fg("accent", "> "),
    hint: (text: string) => theme.fg("dim", text),
  };
}

export function homeRelative(filePath: string, home = os.homedir()): string {
  const normalizedHome = home.replace(/[\\/]+$/, "");
  if (filePath === normalizedHome) return "~";
  const slashPrefix = `${normalizedHome}/`;
  if (filePath.startsWith(slashPrefix)) return `~/${filePath.slice(slashPrefix.length)}`;
  const backslashPrefix = `${normalizedHome}\\`;
  if (filePath.startsWith(backslashPrefix)) return `~\\${filePath.slice(backslashPrefix.length)}`;
  return filePath;
}

export function formatScopeDescription(
  scope: AciConfigScope,
  cwd: string,
  home = os.homedir(),
  providerId?: string,
): string {
  const filePath =
    scope === "project"
      ? getProjectAciCloudConfigPath(cwd, providerId)
      : getGlobalAciCloudConfigPath(home, providerId);
  const displayPath = scope === "home" ? homeRelative(filePath, home) : relative(cwd, filePath);
  return `Writes to the ${scope} config file: ${displayPath}`;
}

export function settingsTitle(label: string): string {
  return `${label} settings (provider v${PROVIDER_VERSION})`;
}

export function modelRegistrationSummary(config: AciCloudConfig): string {
  const tee = config.models.isTeeOnly ? "TEE-only" : "all models";
  const allow = config.models.allowlist?.length
    ? `, allowlist: ${config.models.allowlist.length}`
    : "";
  return `Models: ${tee}${allow}`;
}
