import React, { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useAccountLogin } from "../lib/use-account-login";
import { AccountTools } from "../components/account-tools";
import { ToggleGroup, ToggleGroupItem } from "../components/ui/toggle-group";
import { errorMessage } from "../lib/error-message";
import { Check, Copy, LoaderCircle, Pencil, Plus, TriangleAlert, Trash2 } from "lucide-react";
import { Button } from "../components/ui/button";
import { ActionItem } from "../components/action-item";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../components/ui/tabs";
import { ProfileTransfer } from "../components/maintenance";
import { Field, FieldGroup, FieldLabel, FieldError } from "../components/ui/field";
import { Alert, AlertDescription } from "../components/ui/alert";
import { Input } from "../components/ui/input";
import { IconButton } from "../components/controls";
import { Sheet, SheetActions } from "../components/sheet";
import { FormField } from "../components/settings";
import { ChoiceSelect } from "../components/choice-select";
import type { ConfidentialProfile, ConfidentialProfileInput, GatewayState } from "../../shared/contracts";
import { desktopApi, previewMode } from "../lib/environment";
import { profileHasCredential, profileIsAvailable } from "../lib/protection";
import { ServiceLogo } from "../components/brand";
import { serviceHost } from "../lib/format";
import { SERVICE_PRESETS, servicePreset } from "../lib/services";
import type { ServicePreset } from "../lib/services";

export function ProfilesSheet({
  state,
  busy,
  running,
  initialEditorProfileId,
  startAfterSave = false,
  onSave,
  onActivate,
  onDelete,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  initialEditorProfileId?: string;
  startAfterSave?: boolean;
  onSave(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onActivate(profileId: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [editor, setEditor] = useState<{ kind: "new" } | { kind: "edit"; profileId: string } | undefined>(() => {
    if (state.profiles.length === 0) return { kind: "new" };
    return initialEditorProfileId ? { kind: "edit", profileId: initialEditorProfileId } : undefined;
  });
  const completeEditor = () => setEditor(undefined);
  const [openError, setOpenError] = useState<string>();
  const openEditor = (profileId?: string) => {
    if (previewMode) {
      setEditor(profileId ? { kind: "edit", profileId } : { kind: "new" });
      return;
    }
    setOpenError(undefined);
    void desktopApi.openNativeDialog("profile-editor", { profileId }).catch((error: unknown) => setOpenError(errorMessage(error)));
  };
  return (
    <>
      {state.profiles.length > 0 && (
        <ProfileListSheet
          state={state}
          busy={busy}
          running={running}
          onActivate={onActivate}
          onNew={() => openEditor()}
          onEdit={openEditor}
          error={openError}
          onClose={onClose}
        />
      )}
      {(editor || state.profiles.length === 0) && (
        <ProfileEditorSheet
          state={state}
          busy={busy}
          running={running}
          profile={editor?.kind === "edit" ? state.profiles.find((profile) => profile.id === editor.profileId) : undefined}
          startAfterSave={startAfterSave}
          onSave={onSave}
          onDelete={onDelete}
          onComplete={state.profiles.length === 0 ? onClose : completeEditor}
          onDeleted={state.profiles.length === 1 ? onClose : completeEditor}
          onClose={state.profiles.length === 0 ? onClose : completeEditor}
        />
      )}
    </>
  );
}

function ProfileListSheet({
  state,
  busy,
  running,
  onActivate,
  onNew,
  onEdit,
  onClose,
  error: openError,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  onActivate(profileId: string): Promise<string | undefined>;
  onNew(): void;
  onEdit(profileId: string): void;
  onClose(): void;
  error?: string;
}): React.JSX.Element {
  const [transferBusy, setTransferBusy] = useState(false);
  const [transferMessage, setTransferMessage] = useState<string>();
  const frozen = busy || transferBusy;
  const [workingProfileId, setWorkingProfileId] = useState<string>();
  const [error, setError] = useState<string>();
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const activeProfileAvailable = profileIsAvailable(activeProfile, state);

  const activate = async (profileId: string): Promise<boolean> => {
    if (profileId === state.activeProfileId) return true;
    setWorkingProfileId(profileId);
    setError(undefined);
    const message = await onActivate(profileId);
    setWorkingProfileId(undefined);
    if (message) {
      setError(message);
      return false;
    }
    return true;
  };
  const select = async (profileId: string) => {
    if (!await activate(profileId)) return;
    onClose();
  };
  return (
    <Sheet title="Profiles" className="profiles-sheet w-[min(560px,_calc(var(--window-dialog-width,_100vw)_-_32px))] h-[min(500px,_calc(var(--window-dialog-height,_100vh)_-_32px))] [&[open]]:flex [&[open]]:flex-col" dismissible={!workingProfileId && !transferBusy} onClose={onClose}>
      <p className="sheet-text mt-3 text-sm [&.error]:text-destructive">Choose the service used when protection starts.</p>
      {!activeProfileAvailable && (
        <p className="banner pt-2.25 pr-3 pb-2.25 pl-3 flex items-start gap-1.75 text-destructive bg-[var(--danger-bg)] rounded-lg wrap-anywhere sheet-banner mt-2.5 profile-availability text-warning bg-[var(--warning-bg)]">
          <TriangleAlert size={15} aria-hidden="true" />
          {activeProfile ? `Sign in or add an API key for “${activeProfile.name}” to start protection.` : "Add a profile to start protection."}
        </p>
      )}
      <div className="profile-list min-h-0 mt-3.5 flex-auto overflow-auto bg-card border border-border rounded-2xl" role="list" aria-label="AI service profiles">
        {state.profiles.map((profile) => {
          const active = profile.id === state.activeProfileId;
          const working = profile.id === workingProfileId;
          const status = profileIsAvailable(profile, state) ? "Ready" : "Sign in or add an API key";
          return (
            <div className={`profile-list-row min-w-0 grid grid-cols-[minmax(0,_1fr)_52px] items-center border-b border-b-border [&.is-active]:bg-muted [&.is-active_.profile-select]:bg-transparent last:border-b-0 [&_>_button:last-child]:justify-self-center ${active ? " is-active" : ""}`} role="listitem" key={profile.id}>
              <ActionItem
                type="button"
                className="profile-select min-w-0 [&_>_span:nth-child(2)]:min-w-0 [&_>_span:nth-child(2)]:flex-auto [&_>_span:nth-child(2)]:grid [&_>_span:nth-child(2)]:gap-0.5 [&_strong]:min-w-0 [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_small]:min-w-0 [&_small]:overflow-hidden [&_small]:text-ellipsis [&_small]:whitespace-nowrap [&_strong]:text-foreground [&_strong]:text-sm [&_strong]:font-semibold [&_small]:text-muted-foreground [&_small]:text-xs [&_>_svg]:flex-none [&_>_svg]:text-foreground"
                aria-pressed={active}
                disabled={frozen || Boolean(workingProfileId)}
                onClick={() => void select(profile.id)}
              >
                <ServiceLogo url={profile.remoteUrl} size="large" />
                <span><strong>{profile.name}</strong><small>{serviceHost(profile.remoteUrl)} · {status}</small></span>
                {working ? <LoaderCircle className="is-spinning animate-control-spin motion-reduce:animate-none" size={16} aria-hidden="true" /> : active ? <Check size={16} aria-hidden="true" /> : null}
              </ActionItem>
              <IconButton size="icon-sm" aria-haspopup="dialog" label={`Edit ${profile.name}`} disabled={frozen || Boolean(workingProfileId)} onClick={() => onEdit(profile.id)}><Pencil /></IconButton>
            </div>
          );
        })}
      </div>
      {(error || openError) && <Alert variant="destructive"><AlertDescription>{error || openError}</AlertDescription></Alert>}
      {transferMessage && <p role="status" className="text-sm text-muted-foreground">{transferMessage}</p>}
      <SheetActions leading={
        <div className="flex items-center gap-2">
        <Button type="button" variant="outline" aria-haspopup="dialog" disabled={frozen || Boolean(workingProfileId)} onClick={onNew}><Plus size={15} />New Profile</Button>
        <ProfileTransfer api={desktopApi} disabled={busy || Boolean(workingProfileId)} onBusy={setTransferBusy} onMessage={(message, failed) => { setError(failed ? message : undefined); setTransferMessage(failed ? undefined : message); }} />
        </div>
      }>
        <Button type="button" variant="outline" disabled={Boolean(workingProfileId) || transferBusy} onClick={onClose}>Done</Button>
      </SheetActions>
    </Sheet>
  );
}

export function ProfileEditorSheet({
  state,
  busy,
  running,
  profile,
  startAfterSave = false,
  onSave,
  onDelete,
  onComplete,
  onDeleted,
  onClose,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  profile?: ConfidentialProfile;
  startAfterSave?: boolean;
  onSave(profile: ConfidentialProfileInput, key?: string): Promise<string | undefined>;
  onDelete(profileId: string): Promise<string | undefined>;
  onComplete(): void;
  onDeleted(): void;
  onClose(): void;
}): React.JSX.Element {
  const frozen = busy;
  const isNew = !profile;
  const [draft, setDraft] = useState<ConfidentialProfileInput>(() => ({
    id: profile?.id ?? `profile-${crypto.randomUUID()}`,
    name: profile?.name ?? "Phala",
    provider: profile?.provider ?? "phala",
    remoteUrl: profile?.remoteUrl ?? "https://inference.phala.com",
  }));
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const saveInFlight = useRef(false);
  const autoSaveAttempt = useRef<string | undefined>(undefined);
  const [authMethod, setAuthMethod] = useState<"account" | "apiKey">(profile?.auth.kind === "apiKey" ? "apiKey" : "account");
  const [error, setError] = useState<string>();
  const reportLoginError = useCallback((error: unknown) => setError(errorMessage(error)), []);
  const account = useAccountLogin(desktopApi, reportLoginError);
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
      if (running && !await desktopApi.confirm({ title: "Connect and restart protection?", message: "Connecting this account restarts protection. In-flight requests may be interrupted.", confirmLabel: "Continue" })) return;
      await account.start(draft);
    } catch (error) { setError(errorMessage(error)); }
  };
  const selectedPreset = SERVICE_PRESETS.find((service) => service.id === draft.provider);
  const keyLabel = selectedPreset?.keyLabel ?? "API key";
  const draftUrl = draft.remoteUrl.trim().replace(/\/$/, "");
  const profileChanged = !profile
    || profile.provider !== draft.provider
    || profile.remoteUrl.replace(/\/$/, "") !== draftUrl;
  const savedCredentialApplies = !isNew
    && profileHasCredential(profile)
    && !profileChanged
    && profile.auth.kind === (authMethod === "account" ? "oauth" : "apiKey");
  const detailsEnabled = savedCredentialApplies && draft.provider === "redpill" && authMethod === "account" && Boolean(profile?.id);
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
  const needsWorkspace = Boolean(authorized && draft.provider === "redpill" && workspaces?.length && workspaceId === undefined);

  const savedAccount = currentDetails?.auth.kind === "oauth" ? {
    ...currentDetails.auth,
    scope: { organizationId: currentDetails.auth.scope?.organizationId ?? null, organizationSlug: currentDetails.auth.scope?.organizationSlug ?? null, organization: currentDetails.auth.scope?.organization ?? null,
      workspace: savedScope?.workspace ?? null, workspaceId: savedScope?.workspaceId ?? null },
  } : profile?.auth.kind === "oauth" ? profile.auth : undefined;
  const selectedAccount = authorized?.kind === "oauth" ? authorized
    : !account.busy && savedCredentialApplies ? savedAccount : undefined;
  const accountScope = selectedAccount?.scope;
  const needsAccountLogin = draft.provider !== "custom" && authMethod === "account"
    && !authorized && (!savedCredentialApplies || profile?.auth.kind !== "oauth");


  const chooseService = async (next: ServicePreset) => {
    if (!await account.cancel()) return;
    pendingWorkspaceSave.current = undefined;
    setSelectedWorkspaceId(undefined);
    const preset = SERVICE_PRESETS.find((service) => service.id === next);
    setDraft((current) => ({
      ...current,
      provider: next,
      name: current.name === (SERVICE_PRESETS.find((service) => service.id === current.provider)?.name ?? "Custom")
        ? preset?.name ?? "Custom"
        : current.name,
      remoteUrl: preset?.url ?? (servicePreset(current.remoteUrl) ? "" : current.remoteUrl),
    }));
    setAuthMethod(next === "custom" ? "apiKey" : "account");
    setApiKeyDraft("");
    setError(undefined);
  };
  const chooseAuthMethod = async (next: "account" | "apiKey") => {
    if (!await account.cancel()) return;
    pendingWorkspaceSave.current = undefined;
    setAuthMethod(next);
    setError(undefined);
  };
  const removeProfile = async () => {
    if (working || frozen) return;
    setSaving(true);
    setError(undefined);
    try {
      const current = await desktopApi.getState();
      const needsStop = !current.configurationVerification && ["verified", "blocked", "verifying"].includes(current.status);
      const confirmed = await desktopApi.confirm({
        title: `Delete “${draft.name}”?`,
        message: needsStop ? "Protection will stop and connected agent configurations will be restored. This profile will be deleted and its account credential revoked." : "The profile will be deleted. An account credential will also be revoked at its provider.",
        confirmLabel: needsStop ? "Stop and Delete" : "Delete Profile",
      });
      if (!confirmed) return;
      if (needsStop) await desktopApi.stop();
      const message = await onDelete(draft.id);
      if (message) setError(message);
      else { await account.cancel(); onDeleted(); }
    } catch (error) { setError(errorMessage(error)); }
    finally { setSaving(false); }
  };
  const save = useCallback(async () => {
    if (saveInFlight.current || working || frozen || needsAccountLogin || needsWorkspace) return;
    saveInFlight.current = true;
    setSaving(true);
    setError(undefined);
    try {
      if (!authorized && running && profile?.id === state.activeProfileId && !await desktopApi.confirm({ title: "Save and reconnect?", message: "Saving this active profile restarts protection. In-flight requests may be interrupted.", confirmLabel: "Save and Reconnect" })) return;
      if (!authorized && authMethod === "account" && draft.provider === "redpill"
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
      const message = await onSave(draft, authMethod === "apiKey" || draft.provider === "custom" ? apiKeyDraft.trim() || undefined : undefined);
      if (message) setError(message);
      else onComplete();
    } catch (error) { setError(errorMessage(error)); }
    finally { saveInFlight.current = false; setSaving(false); }
  }, [working, frozen, needsAccountLogin, needsWorkspace, authorized, running, profile?.id, state.activeProfileId, login, authMethod, draft, state.config.requireProductionOs, workspaceId, account.consume, startAfterSave, onComplete, onSave, apiKeyDraft, savedScope?.workspaceId, account.start]);

  useEffect(() => {
    if (!authorized || !login || working || frozen || autoSaveAttempt.current === login.id) return;
    const pendingWorkspace = pendingWorkspaceSave.current;
    if (draft.provider !== "phala" && pendingWorkspace === undefined) return;
    pendingWorkspaceSave.current = undefined;
    autoSaveAttempt.current = login.id;
    if (pendingWorkspace !== undefined && !workspaces?.some((item) => item.id === pendingWorkspace)) {
      setSelectedWorkspaceId(undefined);
      setError("The selected workspace is no longer available. Choose a workspace and save again.");
      return;
    }
    void save();
  }, [authorized, login, draft.provider, workspaces, working, frozen, save]);

  return (
    <Sheet title={isNew ? "New profile" : "Edit profile"} className="profile-editor-sheet w-[min(480px,_calc(var(--window-dialog-width,_100vw)_-_32px))] [&_.sheet-scroll]:min-h-0 [&_.sheet-scroll]:overflow-y-auto [&_.sheet-footer]:flex-none [&[open]]:flex [&[open]]:flex-col [&_form]:min-h-0 [&_form]:flex [&_form]:flex-col form-sheet [&_>_.sheet-heading]:px-5 [&_>_.field-note]:mx-5 [&_.sheet-footer]:mx-5 [&_form_>_[data-slot=field-error]]:mx-5 [&_.sheet-scroll]:px-5" dismissible={!saving && !account.working} onClose={() => void closeEditor()}>
      <form className="mt-4" onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <div className="sheet-scroll py-1">
        <FieldGroup className="gap-4 [&_[data-slot=field]]:gap-2">
        <Field>
        <FieldLabel id="profile-provider-label">Provider</FieldLabel>
        <ToggleGroup variant="outline" className="service-presets w-full grid grid-cols-3 gap-2 max-[440px]:grid-cols-1" value={[draft.provider]} disabled={frozen || working} aria-labelledby="profile-provider-label" onValueChange={([value]) => { if (value === "phala" || value === "redpill" || value === "custom") void chooseService(value); }}>
          {SERVICE_PRESETS.map((service) => (
            <ToggleGroupItem key={service.id} value={service.id} className="service-preset min-w-0 text-left [&_.service-logo]:w-4.5 [&_.service-logo]:h-4.5 [&_.service-custom-icon]:w-4.5 [&_.service-custom-icon]:h-4.5 [&_strong]:min-w-0 [&_strong]:flex-auto [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_>_svg]:flex-none [&_>_svg]:text-foreground" aria-label={service.name}>
              <ServiceLogo url={service.url} />
              <strong>{service.name}</strong>
              {draft.provider === service.id && <Check size={15} aria-hidden="true" />}
            </ToggleGroupItem>
          ))}
          <ToggleGroupItem value="custom" className="service-preset min-w-0 text-left [&_.service-logo]:w-4.5 [&_.service-logo]:h-4.5 [&_.service-custom-icon]:w-4.5 [&_.service-custom-icon]:h-4.5 [&_strong]:min-w-0 [&_strong]:flex-auto [&_strong]:overflow-hidden [&_strong]:text-ellipsis [&_strong]:whitespace-nowrap [&_>_svg]:flex-none [&_>_svg]:text-foreground" aria-label="Custom">
            <ServiceLogo url="custom://service" />
            <strong>Custom</strong>
            {draft.provider === "custom" && <Check size={15} aria-hidden="true" />}
          </ToggleGroupItem>
        </ToggleGroup>
        </Field>
          <FormField id="profile-name" label="Profile name"><Input id="profile-name" value={draft.name} onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))} disabled={frozen || working} autoComplete="off" /></FormField>
          {draft.provider === "custom" && <FormField id="profile-endpoint" label="Service endpoint"><Input id="profile-endpoint" value={draft.remoteUrl} onChange={(event) => setDraft((current) => ({ ...current, remoteUrl: event.target.value }))} disabled={frozen || working} spellCheck={false} /></FormField>}
          <Tabs value={draft.provider === "custom" ? "apiKey" : authMethod} className="gap-4"
            onValueChange={(next) => { if (next === "account" || next === "apiKey") void chooseAuthMethod(next); }}>
            {draft.provider !== "custom" && <TabsList aria-label="Sign-in method" className="w-full">
              <TabsTrigger value="account" disabled={working || frozen}>Account</TabsTrigger>
              <TabsTrigger value="apiKey" disabled={working || frozen}>API key</TabsTrigger>
            </TabsList>}
            <TabsContent value="account">
              <FieldGroup className="gap-4">
                {login && !authorized ? <div className="space-y-3">
                  <div className="flex items-center justify-between gap-3" role="status" aria-live="polite">
                  <div className="space-y-1 text-sm"><p>Continue in your browser</p>{login.userCode && <p className="font-mono text-muted-foreground">{login.userCode}</p>}</div>
                  <div className="flex items-center gap-1">
                    <IconButton size="icon-sm" label="Copy sign-in link" onClick={() => void desktopApi.copyText(login.url).catch((error: unknown) => setError(errorMessage(error)))}><Copy aria-hidden /></IconButton>
                    <Button type="button" variant="ghost" size="sm" disabled={account.working} onClick={() => void account.cancel()}>Cancel Sign-in</Button>
                  </div>
                  </div>
                  {draft.provider === "redpill" && <details>
                    <summary className="cursor-pointer text-sm text-muted-foreground">Paste callback link</summary>
                    <div className="mt-3 space-y-2">
                      <FormField id="account-callback" label="Callback URL">
                        <Input id="account-callback" type="password" value={callbackDraft} autoComplete="off" spellCheck={false} disabled={account.working}
                          placeholder="http://127.0.0.1:4181/oauth/callback?…" onChange={(event) => setCallbackDraft(event.target.value)} />
                      </FormField>
                      <Button type="button" variant="outline" disabled={account.working || !callbackDraft.trim()} onClick={() => { const value = callbackDraft; setCallbackDraft(""); setError(undefined); void account.complete(value); }}>Continue</Button>
                    </div>
                  </details>}
                </div> : selectedAccount ? <>
                  <AccountTools key={login?.id ?? draft.id} api={desktopApi} provider={draft.provider}
                    target={authorized && login ? { kind: "login", id: login.id } : { kind: "profile", profileId: draft.id }}
                    scope={accountScope} images={selectedAccount.images} onSignIn={() => void signIn()}
                    credentialRef={profile?.credentialRef} disabled={working || frozen} />
                  {draft.provider === "redpill" && Boolean(workspaces?.length || accountScope?.workspace) && <FormField id="profile-workspace" label="Workspace">
                    <ChoiceSelect id="profile-workspace" label="Workspace" className="w-full" value={workspaceId === undefined ? "" : String(workspaceId)} options={[
                      { value: "", label: "Select workspace", disabled: true },
                      ...(workspaces ?? []).map((workspace) => ({ value: String(workspace.id), label: workspace.name })),
                      ...(!authorized && savedScope?.workspaceId != null && !workspaces?.some((item) => item.id === savedScope.workspaceId)
                        ? [{ value: String(savedScope.workspaceId), label: savedScope.workspace ?? "Current workspace", disabled: true }] : []),
                    ]} disabled={working || frozen || !workspaces?.length} onChange={(value) => setSelectedWorkspaceId(Number(value))} />
                    {!authorized && workspaceError && <FieldError>{workspaceError}</FieldError>}
                  </FormField>}
                </> : <Button type="button" variant="default" size="lg" className="w-full [&_.service-logo]:size-4" disabled={working || frozen || !draft.name.trim()} onClick={() => void signIn()}><ServiceLogo url={draft.remoteUrl} />Sign in with {selectedPreset?.name}</Button>}
              </FieldGroup>
            </TabsContent>
            <TabsContent value="apiKey">
              <FormField id="profile-key" label={keyLabel} description={savedCredentialApplies ? "Leave blank to keep the saved key." : "Stored securely on this device."}>
                <Input id="profile-key" type="password" value={apiKeyDraft} onChange={(event) => setApiKeyDraft(event.target.value)} placeholder={savedCredentialApplies ? "Replace the saved key" : `Paste your ${keyLabel}`} disabled={frozen || working} autoComplete="off" spellCheck={false} aria-describedby="profile-key-note" />
              </FormField>
            </TabsContent>
          </Tabs>
        </FieldGroup>
        </div>
        <FieldError className="mt-3">{error}</FieldError>
        <SheetActions leading={!isNew && <Button type="button" variant="destructive" disabled={working || frozen} onClick={() => void removeProfile()}><Trash2 size={14} />Delete Profile</Button>}>
          <Button type="button" variant="outline" onClick={() => void closeEditor()} disabled={saving || account.working}>Cancel</Button>
          {!needsAccountLogin && <Button type="submit" variant="default" disabled={working || frozen || !draft.name.trim() || !draft.remoteUrl.trim() || needsWorkspace || (!authorized && !savedCredentialApplies && !apiKeyDraft.trim())}>{saving || busy ? "Saving…" : authorized && draft.provider === "phala" ? "Retry" : "Save"}</Button>}
        </SheetActions>
      </form>
    </Sheet>
  );
}
