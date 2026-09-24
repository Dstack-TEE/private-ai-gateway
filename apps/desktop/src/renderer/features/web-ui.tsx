import React, { useState } from "react";
import { Button } from "../components/ui/button";
import { FieldDescription, FieldGroup } from "../components/ui/field";
import { ListenerFields } from "../components/listen-address";
import { Sheet, SheetActions } from "../components/sheet";
import { useErrorAlert } from "../lib/error-alert";
import { localAddressKind } from "../lib/local-api-config";
import { desktopApi } from "../lib/environment";
import type { AppState, WebUiConfig, WebUiStatus } from "../../shared/contracts";

const DEFAULT_LISTENER = { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4182 } as const;

/** The saved settings behind a status, without listener results. */
export function webUiConfig({ enabled, listenAddress, allowNetworkAccess, port, clientHost }: WebUiStatus): WebUiConfig {
  return { enabled, listenAddress, allowNetworkAccess, port, clientHost };
}

export function WebUiSheet({
  state,
  onSave,
  onClose,
}: {
  state: AppState;
  onSave(config: WebUiConfig): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [draft, setDraft] = useState<WebUiConfig>(() => webUiConfig(state.webUi));
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const reportError = useErrorAlert("Web UI action failed");
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setSaving(true);
    try {
      if (!addressKind) {
        reportError("Enter a valid IPv4 or IPv6 listen address.");
        return;
      }
      if (networkAccess && !await desktopApi.confirm({
        title: "Allow network access?",
        message: `Listen on ${draft.listenAddress}:${draft.port}? The web UI uses unencrypted HTTP, and a signed-in browser can change every setting and read the client key. Only use a trusted network, and never expose this port to the internet. An SSH tunnel or Tailscale is safer.`,
        confirmLabel: "Allow and Save",
      })) return;
      const message = await onSave({ ...draft, allowNetworkAccess: networkAccess });
      if (message) reportError(message);
      else onClose();
    } catch (saveError) {
      reportError(saveError);
    } finally {
      setSaving(false);
    }
  };
  return (
    <Sheet title="Web UI settings" className="web-ui-sheet w-[min(520px,_calc(var(--window-dialog-width,_100vw)_-_32px))] h-[min(420px,_calc(var(--window-dialog-height,_100vh)_-_32px))] [&_.sheet-card]:mt-3 form-sheet [&_>_.sheet-heading]:px-5 [&_.sheet-footer]:mx-5 [&_.sheet-scroll]:px-5" dismissible={!saving} onClose={onClose}>
      <form onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-4">
          <FieldGroup>
            <ListenerFields api={desktopApi} idPrefix="web-ui" value={draft} minPort={1} access="need a sign-in link from pap app open --web" clientHostNote="Optional host for sign-in links. Does not change the listener." disabled={saving} onChange={(listener) => setDraft((current) => ({ ...current, ...listener }))} />
            <FieldDescription>Saving restarts the web UI and signs out every browser session.</FieldDescription>
          </FieldGroup>
        </div>
        <SheetActions leading={
          <Button type="button" variant="outline" disabled={saving} onClick={() => setDraft((current) => ({ ...DEFAULT_LISTENER, enabled: current.enabled }))}>Use default</Button>
        }>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>Cancel</Button>
          <Button type="submit" variant="default" disabled={saving}>{saving ? "Saving…" : "Save"}</Button>
        </SheetActions>
      </form>
    </Sheet>
  );
}
