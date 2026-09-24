import React, { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { cliRegistrationQuery } from "../lib/page-queries";
import { errorMessage } from "../lib/error-message";
import { ChevronRight } from "lucide-react";
import { brand } from "../brand/brand";
import { UpdateControl, UpdateChannelControl, useUpdates } from "../updates";
import { Button } from "../components/ui/button";
import { AppearanceControl } from "../components/appearance";
import { ExportDiagnostics } from "../components/maintenance";
import { Item, ItemActions, ItemContent, ItemTitle, ItemDescription } from "../components/ui/item";
import { Alert, AlertDescription, AlertTitle } from "../components/ui/alert";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../components/ui/collapsible";
import { SettingsSection, SettingsList, SettingsLink, SettingsToggle } from "../components/settings";
import type { DistributionCapabilities, AppState, LaunchPreferences, WebUiStatus } from "../../shared/contracts";
import { desktopApi, signOut } from "../lib/environment";
import { parentDirectory, serviceHost } from "../lib/format";
import { localAddressKind } from "../lib/local-api-config";
import type { SettingsTarget } from "../components/navigation";
import { isProtected, profileIsAvailable } from "../lib/protection";

function CliRegistrationControl(): React.JSX.Element {
  const client = useQueryClient();
  const { data: registration, error: readError } = useQuery(cliRegistrationQuery());
  const mutation = useMutation({
    mutationFn: (installed: boolean) => desktopApi.setCliRegistration(installed),
    onMutate: () => client.cancelQueries({ queryKey: ["cli-registration"] }),
    onSuccess: (next) => { client.setQueryData(["cli-registration"], next); },
  });
  const busy = mutation.isPending;
  const error = mutation.error || readError ? errorMessage(mutation.error ?? readError) : undefined;
  const change = async () => {
    if (!registration || busy) return;
    try { await mutation.mutateAsync(!registration.installed); }
    catch { /* The item shows the mutation failure. */ }
  };
  const directory = registration ? parentDirectory(registration.commandPath) : undefined;
  const description = registration?.installed
      ? registration.onPath
        ? `Installed at ${directory}. This app can resolve private-ai-proxy; terminal PATH may differ.`
        : `Installed at ${directory}. Ensure this directory is in your terminal PATH.`
      : directory ? `Default location: ${directory}` : "Command registration is unavailable.";
  const failure = error ?? registration?.startupError;
  return <Item>
    <ItemContent>
      <ItemTitle>private-ai-proxy command</ItemTitle>
      <ItemDescription>{description}</ItemDescription>
      {failure && <ItemDescription role="alert" className="text-destructive">{failure}</ItemDescription>}
    </ItemContent>
    <ItemActions>
      <Button variant="outline" disabled={busy || !registration} onClick={() => void change()}>
        {busy ? "Working…" : registration?.installed ? "Remove" : "Install"}
      </Button>
    </ItemActions>
  </Item>;
}

/** One scannable line, like the Local API row; the sheet holds the controls. */
function webUiSummary(status: WebUiStatus): string {
  if (!status.enabled) return "Off";
  if (!status.passwordSet) return "Set a password to turn it on";
  if (!status.url) return status.error ?? "Starting…";
  const network = localAddressKind(status.listenAddress) !== "loopback";
  return `${status.listenAddress}:${status.port} · ${network ? "Network access over unencrypted HTTP" : "This device only"}`;
}

function SignOutControl({ onSignOut }: { onSignOut(): Promise<void> }): React.JSX.Element {
  const mutation = useMutation({ mutationFn: onSignOut });
  return <Item>
    <ItemContent>
      <ItemTitle>This browser</ItemTitle>
      <ItemDescription>Signing out ends this browser session. Sign in again with the web UI password.</ItemDescription>
      {mutation.error && <ItemDescription role="alert" className="text-destructive">Could not sign out. {errorMessage(mutation.error)}</ItemDescription>}
    </ItemContent>
    <ItemActions>
      <Button variant="outline" disabled={mutation.isPending} onClick={() => mutation.mutate()}>
        {mutation.isPending ? "Signing Out…" : "Sign Out"}
      </Button>
    </ItemActions>
  </Item>;
}

export function SettingsView({
  updates,
  distribution,
  state,
  busy,
  running,
  allowDevelopmentOs,
  locked,
  onPolicy,
  onResetSettings,
  onAboutLink,
  onOpen,
  launchPreferences,
  savingPreference,
  onLaunchPreference,
}: {
  updates: ReturnType<typeof useUpdates>;
  distribution: DistributionCapabilities;
  state: AppState;
  busy: boolean;
  running: boolean;
  allowDevelopmentOs: boolean;
  locked: boolean;
  onPolicy(value: boolean): void;
  onResetSettings(): void;
  onAboutLink(target: "documentation" | "github"): void;
  onOpen(target: SettingsTarget): void;
  launchPreferences?: LaunchPreferences;
  savingPreference: boolean;
  onLaunchPreference(name: keyof LaunchPreferences, enabled: boolean): void;
}): React.JSX.Element {
  const [diagnosticMessage, setDiagnosticMessage] = useState<string>();
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  return (
    <div className="page-body max-w-230 min-h-full mt-0 mr-auto mb-0 ml-auto settings-page">
      {state.configFiles.error && <Alert variant="destructive" className="mb-5">
        <AlertTitle>Settings file not applied</AlertTitle>
        <AlertDescription className="whitespace-pre-wrap font-mono text-xs">{state.configFiles.error}</AlertDescription>
        <AlertDescription>The previous settings stay in effect until the file is fixed.</AlertDescription>
      </Alert>}
      {state.configFiles.warnings.length > 0 && <Alert className="mb-5">
        <AlertTitle>Check your settings</AlertTitle>
        <AlertDescription className="whitespace-pre-wrap font-mono text-xs">{state.configFiles.warnings.join("\n")}</AlertDescription>
      </Alert>}

      <SettingsSection title="General">
          {distribution.launchAtLogin && <SettingsToggle label="Open at Login" checked={launchPreferences?.openAtLogin ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("openAtLogin", !launchPreferences?.openAtLogin)} />}
          <SettingsToggle label="Protect on launch" checked={launchPreferences?.connectOnLaunch ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("connectOnLaunch", !launchPreferences?.connectOnLaunch)} />
          <AppearanceControl />
          {distribution.notifications && <SettingsLink title="Notifications" aria-label="Notifications" aria-haspopup="dialog" onClick={() => onOpen("notifications")} />}
      </SettingsSection>
      <SettingsSection title="Connections">
          <SettingsLink title="Profiles" aria-label="Profiles" aria-haspopup="dialog" onClick={() => onOpen("confidential")} description={activeProfile ? `${activeProfile.name} · ${serviceHost(activeProfile.remoteUrl)} · ${isProtected(state) ? "Protected" : profileIsAvailable(activeProfile, state) ? "Ready" : "Connect account or add an API key"}` : "No provider configured"} />
          <SettingsLink title="Local API" description="Listener and client access" aria-label="Local API settings" aria-haspopup="dialog" onClick={() => onOpen("local-api")} />
          {distribution.webUi && state.webUi && <SettingsLink title="Web UI" description={webUiSummary(state.webUi)} aria-label="Web UI settings" aria-haspopup="dialog" onClick={() => onOpen("web-ui")} />}
          {signOut && <SignOutControl onSignOut={signOut} />}
      </SettingsSection>

      <Collapsible className="group mt-5 [&:first-child]:mt-0 settings-advanced [&_[data-slot=collapsible-trigger]]:mb-2 [&_[aria-expanded=true]_>_svg]:rotate-90">
        <CollapsibleTrigger render={<Button variant="ghost" />}><ChevronRight size={15} aria-hidden="true" /><span>Advanced</span></CollapsibleTrigger>
        <CollapsibleContent>
          <SettingsList>
          <SettingsToggle label="Allow development OS" checked={allowDevelopmentOs} developmentMode={allowDevelopmentOs} disabled={locked} onToggle={() => onPolicy(!allowDevelopmentOs)} />
          {distribution.nativeUpdates && <UpdateChannelControl updates={updates} />}
          {distribution.cliRegistration && <CliRegistrationControl />}
          {state.configFiles.configPath && <Item>
            <ItemContent>
              <ItemTitle>Settings file</ItemTitle>
              <ItemDescription className="break-all">{state.configFiles.configPath}. API keys and the web UI password are in credentials.toml beside it.</ItemDescription>
            </ItemContent>
          </Item>}
          <ExportDiagnostics api={desktopApi} onMessage={setDiagnosticMessage} />
          <SettingsLink title="Reset settings" disabled={locked} onClick={onResetSettings} />
          </SettingsList>
        </CollapsibleContent>
      </Collapsible>


      <SettingsSection title="About">
          <UpdateControl updates={updates} productName={brand.productName} desktop={distribution.channel !== "web"} />
          {([ ["documentation", "Documentation"], ["github", "GitHub"] ] as const).map(([target, label]) => <SettingsLink key={target} title={label} external onClick={() => onAboutLink(target)} />)}
      </SettingsSection>
      {diagnosticMessage && <p role="status" className="text-sm text-muted-foreground">{diagnosticMessage}</p>}
    </div>
  );
}
