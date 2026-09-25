import React, { useState } from "react";
import { Button } from "../components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldSeparator, FieldTitle } from "../components/ui/field";
import { Input } from "../components/ui/input";
import { ListenerFields } from "../components/listen-address";
import { FormField, SettingsList, SettingsToggle } from "../components/settings";
import { AppDialog } from "../components/app-dialog";
import { useConfirm } from "../components/confirm";
import { DialogFooter } from "../components/ui/dialog";
import { errorMessage } from "../lib/error-message";
import { localAddressKind } from "../lib/local-api-config";
import { desktopApi, web } from "../lib/environment";
import { DEFAULT_WEB_UI_CONFIG, WEB_UI_PASSWORD_MIN_LENGTH as MIN_PASSWORD_LENGTH, type AppState, type WebUiConfig, type WebUiStatus } from "../../shared/contracts";

/** The saved settings behind a status, without listener results. */
export function webUiConfig({ enabled, listenAddress, allowNetworkAccess, port, clientHost }: WebUiStatus): WebUiConfig {
  return { enabled, listenAddress, allowNetworkAccess, port, clientHost };
}

function sameConfig(left: WebUiConfig, right: WebUiConfig): boolean {
  return left.enabled === right.enabled && left.listenAddress === right.listenAddress
    && left.port === right.port && (left.clientHost ?? "") === (right.clientHost ?? "");
}

export function WebUiDialog({
  state,
  onSave,
  onSetPassword,
  onClose,
}: {
  state: AppState;
  onSave(config: WebUiConfig): Promise<string | undefined>;
  onSetPassword(password: string, currentPassword?: string): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const status = state.webUi;
  const [draft, setDraft] = useState<WebUiConfig>(() => webUiConfig(status));
  const [changingPassword, setChangingPassword] = useState(!status.passwordSet);
  const [currentPassword, setCurrentPassword] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const confirm = useConfirm();
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    try {
      if (!addressKind) {
        setError("Enter a valid IPv4 or IPv6 listen address.");
        return;
      }
      const config = { ...draft, allowNetworkAccess: networkAccess };
      const configChanged = !sameConfig(config, webUiConfig(status));
      const newPassword = changingPassword && Boolean(password || confirmation);
      if (newPassword) {
        if ([...password].length < MIN_PASSWORD_LENGTH) {
          setError(`Use a password of at least ${MIN_PASSWORD_LENGTH} characters.`);
          return;
        }
        if (password !== confirmation) {
          setError("The passwords do not match.");
          return;
        }
        if (web && !currentPassword) {
          setError("Enter the current password to change it.");
          return;
        }
      }
      if (config.enabled && !status.passwordSet && !newPassword) {
        setError("Set a password to turn on the web UI.");
        return;
      }
      if (configChanged && networkAccess && !await confirm({
        title: "Allow network access?",
        message: `Listen on ${draft.listenAddress}:${draft.port}? The web UI uses unencrypted HTTP, and a signed-in browser can change every setting and read the client key. Only use a trusted network, and never expose this port to the internet. An SSH tunnel or Tailscale is safer.`,
        confirmLabel: "Allow and Save",
      })) return;
      if (web && status.enabled && !config.enabled && !await confirm({
        title: "Turn off the web UI?",
        message: "This browser session ends now. Turn the web UI on again from the desktop app or with pap settings set web-ui.enabled true.",
        confirmLabel: "Turn Off",
      })) return;
      if (newPassword) {
        const message = await onSetPassword(password, web ? currentPassword : undefined);
        if (message) {
          setError(message);
          return;
        }
        setChangingPassword(false);
        setCurrentPassword("");
        setPassword("");
        setConfirmation("");
      }
      const message = configChanged ? await onSave(config) : undefined;
      if (message) setError(message);
      else onClose();
    } catch (saveError) {
      setError(errorMessage(saveError));
    } finally {
      setSaving(false);
    }
  };
  const openInBrowser = () => void desktopApi.openWebUi().catch((failure: unknown) => setError(errorMessage(failure)));
  return (
    <AppDialog title="Web UI settings" className="sm:max-w-lg" dismissible={!saving} onClose={onClose}>
      <form className="flex min-h-0 flex-col gap-4" onSubmit={(event) => void submit(event)}>
        <div className="-mx-6 min-h-0 overflow-y-auto px-6 py-1">
          <FieldGroup>
            <SettingsList>
              <SettingsToggle label="Web UI" description="Manage this app from a browser. Browsers sign in with the password below." checked={draft.enabled} disabled={saving} onToggle={() => setDraft((current) => ({ ...current, enabled: !current.enabled }))} />
            </SettingsList>
            <ListenerFields api={desktopApi} idPrefix="web-ui" value={draft} minPort={1} access="are protected only by the web UI password" clientHostNote="Optional host shown in the web UI address. Does not change the listener." disabled={saving} onChange={(listener) => setDraft((current) => ({ ...current, ...listener }))} />
            <FieldDescription>Saving listener changes restarts the web UI and signs out every browser session.</FieldDescription>
            <FieldSeparator />
            {changingPassword ? <>
              {web && <FormField id="web-ui-current-password" label="Current password">
                <Input id="web-ui-current-password" type="password" autoComplete="current-password" value={currentPassword} disabled={saving} onChange={(event) => setCurrentPassword(event.target.value)} />
              </FormField>}
              <FormField id="web-ui-password" label={status.passwordSet ? "New password" : "Password"} description={`At least ${MIN_PASSWORD_LENGTH} characters. Changing it signs out every browser session.`}>
                <Input id="web-ui-password" aria-describedby="web-ui-password-note" type="password" autoComplete="new-password" value={password} disabled={saving} onChange={(event) => setPassword(event.target.value)} />
              </FormField>
              <FormField id="web-ui-password-confirmation" label="Confirm password">
                <Input id="web-ui-password-confirmation" type="password" autoComplete="new-password" value={confirmation} disabled={saving} onChange={(event) => setConfirmation(event.target.value)} />
              </FormField>
            </> : <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>Password</FieldTitle>
                <FieldDescription>Set. Changing it signs out every browser session.</FieldDescription>
              </FieldContent>
              <Button type="button" variant="outline" disabled={saving} onClick={() => setChangingPassword(true)}>Change Password</Button>
            </Field>}
            <FieldSeparator />
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>Sign in</FieldTitle>
                <FieldDescription>
                  {status.url ? `Browsers open ${status.url} and enter the password.` : "Turn on the web UI to get its address."}
                  {!web && <> From a terminal: <code>pap app open --web</code>.</>}
                </FieldDescription>
              </FieldContent>
              {!web && <Button type="button" variant="outline" disabled={saving || !status.url} onClick={openInBrowser}>Open in Browser</Button>}
            </Field>
          </FieldGroup>
        </div>
        <FieldError>{error}</FieldError>
        <DialogFooter>
          <Button type="button" variant="outline" className="sm:mr-auto" disabled={saving} onClick={() => setDraft((current) => ({ ...DEFAULT_WEB_UI_CONFIG, enabled: current.enabled }))}>Use Default</Button>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>Cancel</Button>
          <Button type="submit" variant="default" disabled={saving}>{saving ? "Saving…" : "Save"}</Button>
        </DialogFooter>
      </form>
    </AppDialog>
  );
}
