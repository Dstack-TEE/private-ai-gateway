import React, { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, Eye, EyeOff, RefreshCw } from "lucide-react";
import { Button } from "../components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel, FieldSeparator, FieldTitle } from "../components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "../components/ui/input-group";
import { Hint } from "../components/hint";
import { ListenerFields } from "../components/listen-address";
import { SettingsList, SettingsToggle } from "../components/settings";
import { AppDialog, type DialogControl } from "../components/app-dialog";
import { useConfirm } from "../components/confirm";
import { DialogFooter } from "../components/ui/dialog";
import { errorMessage } from "../lib/error-message";
import { localAddressKind } from "../lib/local-api-config";
import { desktopApi, web } from "../lib/environment";
import { DEFAULT_WEB_UI_CONFIG, WEB_UI_PASSWORD_MIN_LENGTH as MIN_PASSWORD_LENGTH, type AppState, type WebUiConfig, type WebUiStatus } from "../../shared/contracts";

const PASSWORD_LABEL = "Web UI password";

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
  copied,
  onCopy,
  onSave,
  onSetPassword,
  onClose,
  ...control
}: {
  state: AppState;
  copied?: string;
  /** Rejects when the value was not copied. */
  onCopy(label: string, value: string): Promise<void>;
  onSave(config: WebUiConfig): Promise<string | undefined>;
  onSetPassword(password: string): Promise<string | undefined>;
} & DialogControl): React.JSX.Element {
  const status = state.webUi;
  const [draft, setDraft] = useState<WebUiConfig>(() => webUiConfig(status));
  // Browsers never see the password; the desktop app and CLI manage it.
  const client = useQueryClient();
  const { data: savedPassword, error: passwordError } = useQuery({ queryKey: ["web-ui-password"], queryFn: () => desktopApi.getWebUiPassword(), enabled: !web, staleTime: 0 });
  /** What the user typed; `undefined` until they edit the saved password. */
  const [passwordDraft, setPasswordDraft] = useState<string>();
  const password = passwordDraft ?? savedPassword ?? "";
  const [passwordVisible, setPasswordVisible] = useState(false);
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const confirm = useConfirm();
  const copyPassword = async () => {
    setError(undefined);
    try { await onCopy(PASSWORD_LABEL, password); }
    catch (failure) { setError(errorMessage(failure)); }
  };
  const generatePassword = async () => {
    setError(undefined);
    if (!await confirm({
      title: "Generate a new web UI password?",
      message: "Every browser is signed out.",
      confirmLabel: "Generate New Password",
      destructive: true,
    })) return;
    setSaving(true);
    try {
      client.setQueryData(["web-ui-password"], await desktopApi.rotateWebUiPassword());
      setPasswordDraft(undefined);
      setPasswordVisible(true);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setSaving(false);
    }
  };
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    try {
      if (!addressKind) {
        setError("Enter a valid IPv4 or IPv6 listen address.");
        return;
      }
      const passwordChanged = passwordDraft !== undefined && passwordDraft !== (savedPassword ?? "");
      if (passwordChanged && [...passwordDraft].length < MIN_PASSWORD_LENGTH) {
        setError(`Use a password of at least ${MIN_PASSWORD_LENGTH} characters.`);
        return;
      }
      const config = { ...draft, allowNetworkAccess: networkAccess };
      const configChanged = !sameConfig(config, webUiConfig(status));
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
      if (passwordChanged) {
        const message = await onSetPassword(passwordDraft);
        if (message) {
          setError(message);
          return;
        }
        client.setQueryData(["web-ui-password"], passwordDraft);
        setPasswordDraft(undefined);
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
  const visibilityLabel = passwordVisible ? "Hide password" : "Show password";
  return (
    <AppDialog {...control} title="Web UI settings" className="sm:max-w-lg" dismissible={!saving} onClose={onClose}>
      <form className="flex min-h-0 flex-col gap-4" onSubmit={(event) => void submit(event)}>
        <div className="-mx-6 min-h-0 overflow-y-auto px-6 py-1">
          <FieldGroup>
            <SettingsList>
              <SettingsToggle label="Web UI" checked={draft.enabled} disabled={saving} onToggle={() => setDraft((current) => ({ ...current, enabled: !current.enabled }))} />
            </SettingsList>
            <ListenerFields api={desktopApi} idPrefix="web-ui" value={draft} minPort={1} access="are protected only by the web UI password" clientHostNote="Optional host shown in the web UI address. Does not change the listener." disabled={saving} onChange={(listener) => setDraft((current) => ({ ...current, ...listener }))} />
            {!web && <>
              <FieldSeparator />
              <Field>
                <FieldLabel htmlFor="web-ui-password">Password</FieldLabel>
                <InputGroup>
                  <InputGroupInput id="web-ui-password" className="font-mono" type={passwordVisible ? "text" : "password"} autoComplete="new-password" spellCheck={false} value={password} placeholder={savedPassword === null ? "Hidden" : undefined} disabled={saving} onChange={(event) => setPasswordDraft(event.target.value)} />
                  <InputGroupAddon align="inline-end">
                    <Hint content={visibilityLabel}><InputGroupButton size="icon-xs" aria-label={visibilityLabel} disabled={!password} onClick={() => setPasswordVisible((visible) => !visible)}>{passwordVisible ? <EyeOff /> : <Eye />}</InputGroupButton></Hint>
                    <Hint content="Copy password"><InputGroupButton size="icon-xs" aria-label="Copy password" disabled={saving || !password} onClick={() => void copyPassword()}>{copied === PASSWORD_LABEL ? <Check /> : <Copy />}</InputGroupButton></Hint>
                    <Hint content="Generate New Password"><InputGroupButton size="icon-xs" aria-label="Generate New Password" disabled={saving} onClick={() => void generatePassword()}><RefreshCw /></InputGroupButton></Hint>
                  </InputGroupAddon>
                </InputGroup>
                {passwordError && <FieldError>{errorMessage(passwordError)}</FieldError>}
              </Field>
              {status.enabled && <Field orientation="horizontal">
                <FieldContent>
                  <FieldTitle>Address</FieldTitle>
                  {status.url ? <FieldDescription><code>{status.url}</code></FieldDescription> : <FieldError>{status.error}</FieldError>}
                </FieldContent>
                <Button type="button" variant="outline" disabled={saving || !status.url} onClick={openInBrowser}>Open in Browser</Button>
              </Field>}
            </>}
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
