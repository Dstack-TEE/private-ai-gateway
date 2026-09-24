import React, { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { cliRegistrationQuery } from "../lib/page-queries";
import { errorMessage } from "../lib/error-message";
import { ChevronRight } from "lucide-react";
import { brand } from "../generated/brand";
import { UpdateControl, UpdateChannelControl, useUpdates } from "../updates";
import { Button } from "../components/ui/button";
import { AppearanceControl } from "../components/appearance";
import { ExportDiagnostics } from "../components/maintenance";
import { ErrorAlert } from "../components/error-alert";
import { Item, ItemActions, ItemContent, ItemTitle, ItemDescription } from "../components/ui/item";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "../components/ui/collapsible";
import { SettingsSection, SettingsList, SettingsLink, SettingsToggle } from "../components/settings";
import type { DistributionCapabilities, AppState, LaunchPreferences, WebUiStatus } from "../../shared/contracts";
import { desktopApi, signOut } from "../lib/environment";
import { localEndpoint, parentDirectory, serviceHost } from "../lib/format";
import { localAddressKind } from "../lib/local-api-config";
import { webUiConfig } from "./web-ui";
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
    catch { /* ErrorAlert observes the mutation failure. */ }
  };
  const directory = registration ? parentDirectory(registration.commandPath) : undefined;
  const description = registration?.installed
      ? registration.onPath
        ? `Installed at ${directory}. This app can resolve private-ai-proxy; terminal PATH may differ.`
        : `Installed at ${directory}. Ensure this directory is in your terminal PATH.`
      : directory ? `Default location: ${directory}` : "Command registration is unavailable.";
  return <Item><ErrorAlert title="Command registration failed" error={error ?? registration?.startupError} />
    <ItemContent>
      <ItemTitle>private-ai-proxy command</ItemTitle>
      <ItemDescription>{description}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <Button variant="outline" disabled={busy || !registration} onClick={() => void change()}>
        {busy ? "Working…" : registration?.installed ? "Remove" : "Install"}
      </Button>
    </ItemActions>
  </Item>;
}

function WebUiControl({ status, web, onOpen }: { status: WebUiStatus; web: boolean; onOpen(): void }): React.JSX.Element {
  const mutation = useMutation({
    mutationFn: (enabled: boolean) => desktopApi.saveWebUi({ ...webUiConfig(status), enabled }),
  });
  const change = async () => {
    const enabled = !status.enabled;
    if (!enabled && web && !await desktopApi.confirm({
      title: "Turn off the web UI?",
      message: "This browser session ends now. Turn the web UI on again from the desktop app or with pap settings set webUi true.",
      confirmLabel: "Turn Off",
    })) return;
    try { await mutation.mutateAsync(enabled); }
    catch { /* ErrorAlert observes the mutation failure. */ }
  };
  const description = !status.enabled
    ? `Manage this app from a browser at ${localEndpoint(status) ?? "the configured address"}. Off by default.`
    : status.url
      ? `Listening on ${status.url}. Sign in with pap app open --web.`
      : status.error ?? "Starting…";
  const network = localAddressKind(status.listenAddress) !== "loopback";
  return <>
    <ErrorAlert title="Web UI could not be changed" error={mutation.error ? errorMessage(mutation.error) : undefined} />
    <SettingsToggle label="Web UI" description={description} checked={status.enabled} disabled={mutation.isPending} onToggle={() => void change()} />
    <SettingsLink title="Web UI listener" description={`${status.listenAddress}:${status.port} · ${network ? "Network access over unencrypted HTTP" : "This device only"}`} aria-label="Web UI listener settings" aria-haspopup="dialog" onClick={onOpen} />
  </>;
}

function SignOutControl({ onSignOut }: { onSignOut(): Promise<void> }): React.JSX.Element {
  const mutation = useMutation({ mutationFn: onSignOut });
  return <Item><ErrorAlert title="Could not sign out" error={mutation.error ? errorMessage(mutation.error) : undefined} />
    <ItemContent>
      <ItemTitle>This browser</ItemTitle>
      <ItemDescription>Signing out ends this browser session. Run pap app open --web to sign in again.</ItemDescription>
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

      <SettingsSection title="General">
          {distribution.launchAtLogin && <SettingsToggle label="Open at Login" checked={launchPreferences?.openAtLogin ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("openAtLogin", !launchPreferences?.openAtLogin)} />}
          <SettingsToggle label="Protect on launch" checked={launchPreferences?.connectOnLaunch ?? false} disabled={!launchPreferences || savingPreference} onToggle={() => onLaunchPreference("connectOnLaunch", !launchPreferences?.connectOnLaunch)} />
          <AppearanceControl />
          {distribution.notifications && <SettingsLink title="Notifications" aria-label="Notifications" aria-haspopup="dialog" onClick={() => onOpen("notifications")} />}
      </SettingsSection>
      <SettingsSection title="Connections">
          <SettingsLink title="Profiles" aria-label="Profiles" aria-haspopup="dialog" onClick={() => onOpen("confidential")} description={activeProfile ? `${activeProfile.name} · ${serviceHost(activeProfile.remoteUrl)} · ${isProtected(state) ? "Protected" : profileIsAvailable(activeProfile, state) ? "Ready" : "Connect account or add an API key"}` : "No provider configured"} />
          <SettingsLink title="Local API" description="Listener and client access" aria-label="Local API settings" aria-haspopup="dialog" onClick={() => onOpen("local-api")} />
          {distribution.webUi && state.webUi && <WebUiControl status={state.webUi} web={distribution.channel === "web"} onOpen={() => onOpen("web-ui")} />}
          {signOut && <SignOutControl onSignOut={signOut} />}
      </SettingsSection>

      <Collapsible className="group mt-5 [&:first-child]:mt-0 settings-advanced [&_[data-slot=collapsible-trigger]]:mb-2 [&_[aria-expanded=true]_>_svg]:rotate-90">
        <CollapsibleTrigger render={<Button variant="ghost" />}><ChevronRight size={15} aria-hidden="true" /><span>Advanced</span></CollapsibleTrigger>
        <CollapsibleContent>
          <SettingsList>
          <SettingsToggle label="Allow development OS" checked={allowDevelopmentOs} developmentMode={allowDevelopmentOs} disabled={locked} onToggle={() => onPolicy(!allowDevelopmentOs)} />
          {distribution.nativeUpdates && <UpdateChannelControl updates={updates} />}
          {distribution.cliRegistration && <CliRegistrationControl />}
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
