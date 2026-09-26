import React, { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useAccountLogin } from "../lib/use-account-login";
import { AccountTools } from "../components/account-tools";
import { ToggleGroup, ToggleGroupItem } from "../components/ui/toggle-group";
import { errorMessage } from "../lib/error-message";
import { Check, Copy, ExternalLink, LoaderCircle, Pencil, Plus, TriangleAlert, Trash2 } from "lucide-react";
import { Button } from "../components/ui/button";
import { ActionItem } from "../components/action-item";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../components/ui/tabs";
import { ProfileTransfer } from "../components/maintenance";
import { Field, FieldError, FieldGroup, FieldLabel } from "../components/ui/field";
import { Alert, AlertDescription } from "../components/ui/alert";
import { Input } from "../components/ui/input";
import { IconButton } from "../components/controls";
import { AppDialog, useDialog, type DialogControl } from "../components/app-dialog";
import { useConfirm } from "../components/confirm";
import { DialogFooter } from "../components/ui/dialog";
import { FormField } from "../components/settings";
import { ChoiceSelect } from "../components/choice-select";
import { DEFAULT_SERVICE_PROVIDER, SERVICE_PROVIDERS, type ConfidentialProfile, type ConfidentialProfileInput, type AppState, type ServiceProvider } from "../../shared/contracts";
import { desktopApi, distributionCapabilities } from "../lib/environment";
import { profileIsAvailable } from "../lib/protection";
import { ServiceLogo } from "../components/brand";
import { serviceHost } from "../lib/format";

export function ProfilesDialog({
  state,
  repair,
  onActivate,
  onSave,
  onDelete,
  onClose,
  ...control
}: {
  state: AppState;
  /** Opens the active profile's editor on top, for protection that needs its credential. */
  repair: boolean;
  onActivate(profileId: string): Promise<string | undefined>;
  onSave(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
} & DialogControl): React.JSX.Element {
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const editor = useDialog<{ profile?: ConfidentialProfile }>(repair && activeProfile ? { profile: activeProfile } : undefined);
  const openedProfile = editor.payload?.profile;
  const liveProfile = openedProfile && state.profiles.find((profile) => profile.id === openedProfile.id);
  // Deleting the profile, in the editor or elsewhere, closes its editor, which
  // shows the profile as it was opened until it has closed.
  if (openedProfile && !liveProfile && editor.control.open) editor.control.onClose();
  const editingProfile = liveProfile ?? openedProfile;
  const newProfileButton = useRef<HTMLButtonElement>(null);
  const [transferBusy, setTransferBusy] = useState(false);
  const [transferMessage, setTransferMessage] = useState<string>();
  const [error, setError] = useState<string>();
  const busy = state.status === "verifying";
  const frozen = busy || transferBusy;
  const [workingProfileId, setWorkingProfileId] = useState<string>();
  const activeProfileAvailable = profileIsAvailable(activeProfile, state);
  const activeConnection = activeProfile && connectionRequirement(activeProfile);

  const select = async (profileId: string) => {
    if (profileId === state.activeProfileId) {
      onClose();
      return;
    }
    setWorkingProfileId(profileId);
    setError(undefined);
    try {
      const message = await onActivate(profileId);
      if (message) setError(message);
      else onClose();
    } finally {
      setWorkingProfileId(undefined);
    }
  };
  return (
    <AppDialog {...control} title="Profiles" className="sm:max-w-xl" dismissible={!workingProfileId && !transferBusy} onClose={onClose}>
      <p className="text-sm">Choose the service used when protection starts.</p>
      {!activeProfileAvailable && (
        <p className="banner profile-availability flex items-start gap-1.75 rounded-lg bg-[var(--warning-bg)] px-3 py-2.25 text-warning wrap-anywhere">
          <TriangleAlert size={15} aria-hidden="true" />
          {activeProfile ? `${activeConnection} for “${activeProfile.name}” to start protection.` : "Add a profile to start protection."}
        </p>
      )}
      {state.profiles.length > 0 && <div className="profile-list min-h-0 flex-auto overflow-auto bg-card border border-border rounded-2xl" role="list" aria-label="AI service profiles">
        {state.profiles.map((profile) => {
          const active = profile.id === state.activeProfileId;
          const working = profile.id === workingProfileId;
          const status = profileIsAvailable(profile, state) ? "Ready" : connectionRequirement(profile);
          return (
            <div className={`profile-list-row min-w-0 grid grid-cols-[minmax(0,_1fr)_52px] items-center border-b border-b-border [&.is-active]:bg-muted [&.is-active_.profile-select]:bg-transparent last:border-b-0 [&_>_button:last-child]:justify-self-center ${active ? " is-active" : ""}`} role="listitem" key={profile.id}>
              <ActionItem
                type="button"
                className="profile-select min-w-0 [&_>_span:nth-child(2)]:min-w-0 [&_>_span:nth-child(2)]:flex-auto [&_>_span:nth-child(2)]:grid [&_>_span:nth-child(2)]:gap-0.5 [&_strong]:min-w-0 [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_small]:min-w-0 [&_small]:overflow-hidden [&_small]:text-ellipsis [&_small]:whitespace-nowrap [&_strong]:text-foreground [&_strong]:text-sm [&_strong]:font-semibold [&_small]:text-muted-foreground [&_small]:text-xs [&_>_svg]:flex-none [&_>_svg]:text-foreground"
                aria-pressed={active}
                disabled={frozen || Boolean(workingProfileId)}
                onClick={() => void select(profile.id)}
              >
                <ServiceLogo provider={profile.provider} size="large" />
                <span><strong>{profile.name}</strong><small>{serviceHost(profile.remoteUrl)} · {status}</small></span>
                {working ? <LoaderCircle className="is-spinning animate-control-spin motion-reduce:animate-none" size={16} aria-hidden="true" /> : active ? <Check size={16} aria-hidden="true" /> : null}
              </ActionItem>
              <IconButton size="icon-sm" aria-haspopup="dialog" label={`Edit ${profile.name}`} disabled={frozen || Boolean(workingProfileId)} onClick={() => editor.show({ profile })}><Pencil /></IconButton>
            </div>
          );
        })}
      </div>}
      {transferMessage && <p role="status" className="text-sm text-muted-foreground">{transferMessage}</p>}
      {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
      <DialogFooter>
        <div className="flex items-center gap-2 sm:mr-auto">
          <Button ref={newProfileButton} type="button" variant="outline" aria-haspopup="dialog" disabled={frozen || Boolean(workingProfileId)} onClick={() => editor.show({})}><Plus size={15} />New Profile</Button>
          <ProfileTransfer api={desktopApi} disabled={busy || Boolean(workingProfileId)} onBusy={setTransferBusy} onMessage={(message, failed) => { setError(failed ? message : undefined); setTransferMessage(failed ? undefined : message); }} />
        </div>
        <Button type="button" variant="outline" disabled={Boolean(workingProfileId) || transferBusy} onClick={onClose}>Done</Button>
      </DialogFooter>
      {editor.payload && <ProfileEditorDialog
        key={editor.key} state={state} profile={editingProfile} {...editor.control} finalFocus={liveProfile || !openedProfile ? undefined : newProfileButton}
        onSave={onSave} onDelete={onDelete} onComplete={editor.control.onClose}
      />}
    </AppDialog>
  );
}

function connectionRequirement(profile: Pick<ConfidentialProfile, "provider">): string {
  const provider = SERVICE_PROVIDERS[profile.provider];
  return provider.accountLogin ? `Connect ${provider.label} or add an API key` : "Add an API key";
}

/** A v4 UUID; `crypto.randomUUID` is missing outside secure contexts, such as a web UI on a LAN address. */
function randomUuid(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** Its owner closes it when the profile is deleted, as the profile leaves `state`. */
export function ProfileEditorDialog({
  state,
  profile,
  startAfterSave = false,
  onSave,
  onDelete,
  onComplete,
  onClose,
  ...control
}: {
  state: AppState;
  profile?: ConfidentialProfile;
  startAfterSave?: boolean;
  onSave(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onComplete(): void;
} & DialogControl): React.JSX.Element {
  const busy = state.status === "verifying";
  // Saving or connecting an account restarts protection that is on.
  const running = state.protection.action.operation === "stop";
  const frozen = busy;
  const isNew = !profile;
  const [draft, setDraft] = useState<ConfidentialProfileInput>(() => {
    const provider = SERVICE_PROVIDERS[profile?.provider ?? DEFAULT_SERVICE_PROVIDER];
    return {
      id: profile?.id ?? `profile-${randomUuid()}`,
      name: profile?.name ?? provider.label,
      provider: provider.id,
      remoteUrl: profile?.remoteUrl ?? provider.presetUrl ?? "",
    };
  });
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const saveInFlight = useRef(false);
  const autoSaveAttempt = useRef<string | undefined>(undefined);
  const [authMethod, setAuthMethod] = useState<"account" | "apiKey">(profile?.auth.kind === "apiKey" ? "apiKey" : "account");
  const [error, setError] = useState<string>();
  const reportError = useCallback((failure: unknown) => setError(errorMessage(failure)), []);
  const confirm = useConfirm();
  const account = useAccountLogin(desktopApi, reportError);
  const { session: login, auth: authorized } = account;
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState<number>();
  const pendingWorkspaceSave = useRef<number | undefined>(undefined);
  const [callbackDraft, setCallbackDraft] = useState("");
  useEffect(() => setCallbackDraft(""), [login?.id]);

  const working = saving || account.busy;
  const closeEditor = async () => {
    if (!saveInFlight.current && await account.cancel()) onClose();
  };
  const signIn = async () => {
    pendingWorkspaceSave.current = undefined;
    setSelectedWorkspaceId(undefined);
    setError(undefined);
    try {
      if (running && !await confirm({ title: "Connect and restart protection?", message: "Connecting this account restarts protection. In-flight requests may be interrupted.", confirmLabel: "Continue" })) return;
      await account.start(draft);
    } catch (error) { reportError(error); }
  };
  const provider = SERVICE_PROVIDERS[draft.provider];
  const keyLabel = provider.keyLabel;
  const draftUrl = draft.remoteUrl.trim().replace(/\/$/, "");
  const profileChanged = !profile
    || profile.provider !== draft.provider
    || profile.remoteUrl.replace(/\/$/, "") !== draftUrl;
  const savedCredentialApplies = !isNew
    && profile.credentialSaved
    && !profileChanged
    && profile.auth.kind === (authMethod === "account" ? "oauth" : "apiKey");
  const detailsEnabled = savedCredentialApplies && provider.workspaces && authMethod === "account" && Boolean(profile?.id);
  const { data: currentDetails, error: detailsError } = useQuery({
    queryKey: detailsEnabled ? ["account-details", profile?.id, profile?.credentialRef] : ["account-details", null],
    queryFn: () => desktopApi.getAccountDetails(profile?.id ?? ""), enabled: detailsEnabled, staleTime: 30_000,
  });
  const workspaceError = detailsError ? errorMessage(detailsError) : undefined;
  const workspaces = account.details?.workspaces ?? currentDetails?.workspaces;
  const savedScope = profile?.auth.kind === "oauth" ? profile.auth.scope : undefined;
  const workspaceId = selectedWorkspaceId ?? (authorized
    ? workspaces?.length === 1 ? workspaces[0]?.id : undefined
    : savedScope?.workspaceId ?? undefined);
  const needsWorkspace = Boolean(authorized && provider.workspaces && workspaces?.length && workspaceId === undefined);

  const savedAccount = currentDetails?.auth.kind === "oauth" ? {
    ...currentDetails.auth,
    scope: { organizationId: currentDetails.auth.scope?.organizationId ?? null, organizationSlug: currentDetails.auth.scope?.organizationSlug ?? null, organization: currentDetails.auth.scope?.organization ?? null,
      workspace: savedScope?.workspace ?? null, workspaceId: savedScope?.workspaceId ?? null },
  } : profile?.auth.kind === "oauth" ? profile.auth : undefined;
  const selectedAccount = authorized?.kind === "oauth" ? authorized
    : !account.busy && savedCredentialApplies ? savedAccount : undefined;
  const accountScope = selectedAccount?.scope;
  const needsAccountLogin = provider.accountLogin && authMethod === "account"
    && !authorized && (!savedCredentialApplies || profile?.auth.kind !== "oauth");


  const chooseService = async (next: ServiceProvider) => {
    if (!await account.cancel()) return;
    pendingWorkspaceSave.current = undefined;
    setSelectedWorkspaceId(undefined);
    const nextProvider = SERVICE_PROVIDERS[next];
    setDraft((current) => {
      const currentProvider = SERVICE_PROVIDERS[current.provider];
      return {
        ...current,
        provider: next,
        name: current.name === currentProvider.label ? nextProvider.label : current.name,
        remoteUrl: nextProvider.presetUrl ?? (currentProvider.presetUrl ? "" : current.remoteUrl),
      };
    });
    setAuthMethod(nextProvider.accountLogin ? "account" : "apiKey");
    setApiKeyDraft("");
  };
  const chooseAuthMethod = async (next: "account" | "apiKey") => {
    if (!await account.cancel()) return;
    pendingWorkspaceSave.current = undefined;
    setAuthMethod(next);
  };
  const removeProfile = async () => {
    if (working || frozen) return;
    setSaving(true);
    setError(undefined);
    try {
      const current = await desktopApi.getState();
      const needsStop = !current.configurationVerification && ["verified", "blocked", "verifying"].includes(current.status);
      const confirmed = await confirm({
        title: `Delete “${draft.name}”?`,
        message: needsStop ? "Protection will stop and connected agent configurations will be restored. This profile will be deleted and its account credential revoked." : "The profile will be deleted. An account credential will also be revoked at its provider.",
        confirmLabel: needsStop ? "Stop and Delete" : "Delete Profile",
        destructive: true,
      });
      if (!confirmed) return;
      if (needsStop) await desktopApi.stop();
      const message = await onDelete(draft.id);
      if (message) reportError(message);
      else await account.cancel();
    } catch (error) { reportError(error); }
    finally { setSaving(false); }
  };
  const save = useCallback(async () => {
    if (saveInFlight.current || working || frozen || needsAccountLogin || needsWorkspace) return;
    saveInFlight.current = true;
    setSaving(true);
    setError(undefined);
    try {
      if (!authorized && running && profile?.id === state.activeProfileId && !await confirm({ title: "Save and reconnect?", message: "Saving this active profile restarts protection. In-flight requests may be interrupted.", confirmLabel: "Save and Reconnect" })) return;
      if (!authorized && authMethod === "account" && provider.workspaces
        && workspaceId !== undefined && workspaceId !== savedScope?.workspaceId) {
        pendingWorkspaceSave.current = workspaceId;
        if (!await account.start(draft)) pendingWorkspaceSave.current = undefined;
        return;
      }
      if (authorized && login && authMethod === "account") {
        const saved = await desktopApi.saveAccountLogin(login.id, draft, state.config.requireProductionOs, workspaceId);
        account.consume();
        if (startAfterSave) await desktopApi.start(saved.config);
        onComplete();
        return;
      }
      const message = await onSave(draft, authMethod === "apiKey" || !provider.accountLogin ? apiKeyDraft.trim() || undefined : undefined);
      if (message) reportError(message);
      else onComplete();
    } catch (error) { reportError(error); }
    finally { saveInFlight.current = false; setSaving(false); }
  }, [working, frozen, needsAccountLogin, needsWorkspace, authorized, running, profile?.id, state.activeProfileId, login, authMethod, draft, provider, state.config.requireProductionOs, workspaceId, account.consume, startAfterSave, onComplete, onSave, apiKeyDraft, savedScope?.workspaceId, account.start, reportError, confirm]);

  useEffect(() => {
    if (!authorized || !login || working || frozen || autoSaveAttempt.current === login.id) return;
    const pendingWorkspace = pendingWorkspaceSave.current;
    // Without a workspace to choose, a completed sign-in saves at once.
    if (provider.workspaces && pendingWorkspace === undefined) return;
    pendingWorkspaceSave.current = undefined;
    autoSaveAttempt.current = login.id;
    if (pendingWorkspace !== undefined && !workspaces?.some((item) => item.id === pendingWorkspace)) {
      setSelectedWorkspaceId(undefined);
      reportError("The selected workspace is no longer available. Choose a workspace and save again.");
      return;
    }
    void save();
  }, [authorized, login, provider, workspaces, working, frozen, save, reportError]);

  return (
    <AppDialog {...control} title={isNew ? "New profile" : "Edit profile"} className="sm:max-w-lg" dismissible={!saving && !account.working} onClose={() => void closeEditor()}>
      <form className="flex min-h-0 flex-col gap-4" onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <div className="-mx-6 min-h-0 overflow-y-auto px-6 py-1">
        <FieldGroup className="gap-4 [&_[data-slot=field]]:gap-2">
        <Field>
        <FieldLabel id="profile-provider-label">Provider</FieldLabel>
        <ToggleGroup variant="outline" className="service-presets w-full grid grid-cols-3 gap-2 max-[440px]:grid-cols-1" value={[draft.provider]} disabled={frozen || working} aria-labelledby="profile-provider-label" onValueChange={([value]) => { const next = Object.values(SERVICE_PROVIDERS).find((service) => service.id === value); if (next) void chooseService(next.id); }}>
          {Object.values(SERVICE_PROVIDERS).map((service) => (
            <ToggleGroupItem key={service.id} value={service.id} className="service-preset min-w-0 text-left [&_.service-logo]:w-4.5 [&_.service-logo]:h-4.5 [&_.service-custom-icon]:w-4.5 [&_.service-custom-icon]:h-4.5 [&_strong]:min-w-0 [&_strong]:flex-auto [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_>_svg]:flex-none [&_>_svg]:text-foreground" aria-label={service.label}>
              <ServiceLogo provider={service.id} />
              <strong>{service.label}</strong>
              {draft.provider === service.id && <Check size={15} aria-hidden="true" />}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        </Field>
          <FormField id="profile-name" label="Profile name"><Input id="profile-name" value={draft.name} onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))} disabled={frozen || working} autoComplete="off" /></FormField>
          {!provider.presetUrl && <FormField id="profile-endpoint" label="Service endpoint" description={<>Requires ACI support. <Button type="button" variant="link" className="h-auto p-0 text-xs align-baseline" onClick={() => { void desktopApi.openAboutLink("aci").catch(reportError); }}>About ACI<ExternalLink size={12} aria-hidden="true" /></Button></>}><Input id="profile-endpoint" aria-describedby="profile-endpoint-note" value={draft.remoteUrl} onChange={(event) => setDraft((current) => ({ ...current, remoteUrl: event.target.value }))} disabled={frozen || working} spellCheck={false} autoComplete="off" /></FormField>}
          <Tabs value={provider.accountLogin ? authMethod : "apiKey"} className="gap-4"
            onValueChange={(next) => { if (next === "account" || next === "apiKey") void chooseAuthMethod(next); }}>
            {provider.accountLogin && <TabsList aria-label="Connection method" className="w-full">
              <TabsTrigger value="account" disabled={working || frozen}>Account</TabsTrigger>
              <TabsTrigger value="apiKey" disabled={working || frozen}>API key</TabsTrigger>
            </TabsList>}
            <TabsContent value="account">
              <FieldGroup className="gap-4">
                {login && !authorized ? <div className="space-y-3">
                  <div className="flex items-center justify-between gap-3" role="status" aria-live="polite">
                  <div className="space-y-1 text-sm"><p>Continue in your browser</p>{login.userCode && <p className="font-mono text-muted-foreground">{login.userCode}</p>}</div>
                  <div className="flex items-center gap-1">
                    <IconButton size="icon-sm" label="Copy connection link" onClick={() => void desktopApi.copyText(login.url).catch(reportError)}><Copy aria-hidden /></IconButton>
                    <Button type="button" variant="ghost" size="sm" disabled={account.working} onClick={() => void account.cancel()}>Cancel</Button>
                  </div>
                  </div>
                  {provider.callbackUrl && <details>
                    <summary className="text-sm text-muted-foreground">Paste callback link</summary>
                    <div className="mt-3 space-y-2">
                      <FormField id="account-callback" label="Callback URL">
                        <Input id="account-callback" type="password" value={callbackDraft} autoComplete="off" spellCheck={false} disabled={account.working}
                          placeholder="http://127.0.0.1:4181/oauth/callback?…" onChange={(event) => setCallbackDraft(event.target.value)} />
                      </FormField>
                      <Button type="button" variant="outline" disabled={account.working || !callbackDraft.trim()} onClick={() => { const value = callbackDraft; setCallbackDraft(""); void account.complete(value); }}>Continue</Button>
                    </div>
                  </details>}
                </div> : selectedAccount ? <>
                  <AccountTools key={login?.id ?? draft.id} api={desktopApi} provider={draft.provider}
                    target={authorized && login ? { kind: "login", id: login.id } : { kind: "profile", profileId: draft.id }}
                    scope={accountScope} images={selectedAccount.images} onSignIn={() => void signIn()}
                    credentialRef={profile?.credentialRef} onError={reportError} disabled={working || frozen} />
                  {provider.workspaces && Boolean(workspaces?.length || accountScope?.workspace) && <FormField id="profile-workspace" label="Workspace">
                    <ChoiceSelect id="profile-workspace" label="Workspace" className="w-full" value={workspaceId === undefined ? "" : String(workspaceId)} options={[
                      { value: "", label: "Select workspace", disabled: true },
                      ...(workspaces ?? []).map((workspace) => ({ value: String(workspace.id), label: workspace.name })),
                      ...(!authorized && savedScope?.workspaceId != null && !workspaces?.some((item) => item.id === savedScope.workspaceId)
                        ? [{ value: String(savedScope.workspaceId), label: savedScope.workspace ?? "Current workspace", disabled: true }] : []),
                    ]} disabled={working || frozen || !workspaces?.length} onChange={(value) => setSelectedWorkspaceId(Number(value))} />
                    {!authorized && <FieldError>{workspaceError && `Could not load account workspaces. ${workspaceError}`}</FieldError>}
                  </FormField>}
                </> : <Button type="button" variant="default" size="lg" className="w-full [&_.service-logo]:size-4" disabled={working || frozen || !draft.name.trim()} onClick={() => void signIn()}><ServiceLogo provider={draft.provider} />Connect {provider.label}</Button>}
              </FieldGroup>
            </TabsContent>
            <TabsContent value="apiKey">
              <FormField id="profile-key" label={keyLabel} description={savedCredentialApplies ? "Leave blank to keep the saved key." : "Stored securely on this device."}>
                <Input id="profile-key" type="password" value={apiKeyDraft} onChange={(event) => setApiKeyDraft(event.target.value)} placeholder={savedCredentialApplies ? "Replace the saved key" : `Paste your ${keyLabel}`} disabled={frozen || working} autoComplete="off" spellCheck={false} aria-describedby="profile-key-note" />
                {distributionCapabilities.accountPortalLinks && provider.accountLogin && <Button type="button" variant="link" size="sm" className="h-auto justify-start self-start p-0" disabled={working || frozen} onClick={() => {
                  void desktopApi.openApiKeyPage(draft.provider).catch(reportError);
                }}>Get API key<ExternalLink size={14} aria-hidden="true" /></Button>}
              </FormField>
            </TabsContent>
          </Tabs>
        </FieldGroup>
        </div>
        <FieldError>{error}</FieldError>
        <DialogFooter>
          {!isNew && <Button type="button" variant="destructive" className="sm:mr-auto" disabled={working || frozen} onClick={() => void removeProfile()}><Trash2 size={14} />Delete Profile</Button>}
          <Button type="button" variant="outline" onClick={() => void closeEditor()} disabled={saving || account.working}>Cancel</Button>
          {!needsAccountLogin && <Button type="submit" variant="default" disabled={working || frozen || !draft.name.trim() || !draft.remoteUrl.trim() || needsWorkspace || (!authorized && !savedCredentialApplies && !apiKeyDraft.trim())}>{saving || busy ? "Saving…" : authorized && !provider.workspaces ? "Retry" : "Save"}</Button>}
        </DialogFooter>
      </form>
    </AppDialog>
  );
}
