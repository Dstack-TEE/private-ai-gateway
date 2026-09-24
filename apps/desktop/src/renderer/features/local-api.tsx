import React, { useState } from "react";
import { Check, Copy, Eye, EyeOff, RefreshCw } from "lucide-react";
import { Button } from "../components/ui/button";
import { ListenerFields } from "../components/listen-address";
import { localAddressKind } from "../lib/local-api-config";
import { Hint } from "../components/hint";
import { Field, FieldGroup, FieldLabel, FieldDescription, FieldSeparator } from "../components/ui/field";
import { useErrorAlert } from "../lib/error-alert";
import { Item } from "../components/ui/item";
import { InputGroup, InputGroupInput, InputGroupAddon, InputGroupButton } from "../components/ui/input-group";
import { IconButton } from "../components/controls";
import { Sheet, SheetActions } from "../components/sheet";
import type { AppState, ListenConfig } from "../../shared/contracts";
import { maskClientKey } from "../lib/format";
import { desktopApi } from "../lib/environment";
import { cn } from "../lib/utils";

export function LocalApiPanel({
  proxyUrl,
  clientKey,
  clientKeyVisible,
  copied,
  onCopy,
  onToggleKey,
}: {
  proxyUrl?: string;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  onCopy(label: string, value: string): Promise<void>;
  onToggleKey(): void;
}): React.JSX.Element {
  const endpointLabel = "Local endpoint";
  const keyLabel = "Client key";
  return (
    <div className="copy-rows relative grid auto-rows-auto gap-3">
      <CopyRow title="Endpoint" copyLabel={endpointLabel} value={proxyUrl} copied={copied} onCopy={onCopy} />
      <CopyRow
        title="Client key"
        copyLabel={keyLabel}
        value={clientKey || undefined}
        displayValue={clientKey ? (clientKeyVisible ? clientKey : maskClientKey(clientKey)) : undefined}
        ariaValue={clientKey ? (clientKeyVisible ? clientKey : "hidden") : undefined}
        copied={copied}
        onCopy={onCopy}
      >
        <IconButton className="row-action relative z-2 ml-auto" label={clientKeyVisible ? "Hide client key" : "Reveal client key"} disabled={!clientKey} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
      </CopyRow>
    </div>
  );
}

function CopyRow({
  title,
  copyLabel,
  value,
  displayValue = value,
  ariaValue = displayValue,
  copied,
  onCopy,
  children,
}: React.PropsWithChildren<{
  title: string;
  copyLabel: string;
  value?: string;
  displayValue?: string;
  ariaValue?: string;
  copied?: string;
  onCopy(label: string, value: string): Promise<void>;
}>): React.JSX.Element {
  const isCopied = copied === copyLabel;
  return (
    <Item variant="muted" size="xs" className="copy-row relative h-14 min-w-0 overflow-hidden">
      <Button
        variant="ghost"
        className="copy-surface absolute inset-0 flex size-full min-h-0 min-w-0 flex-col items-start justify-center gap-0.5 rounded-none border-0 bg-transparent py-2.25 pr-[min(100px,_40%)] pl-3 text-left hover:bg-muted [&:focus-visible_.copy-feedback]:opacity-100 [&:hover_.copy-feedback]:opacity-100 [&_code]:max-w-full [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap"
        disabled={!value}
        aria-label={`${copyLabel}: ${ariaValue ?? "Unavailable"}. Copy`}
        onClick={() => value && void onCopy(copyLabel, value)}
      >
        <span className="row-title text-xs font-normal text-muted-foreground">{title}</span>
        <code className="row-note block w-full flex-none overflow-hidden text-ellipsis whitespace-nowrap text-sm text-foreground">{displayValue ?? "Unavailable"}</code>
        <span className={cn("copy-feedback absolute right-13.5 top-1/2 -translate-y-1/2 text-xs font-semibold text-muted-foreground opacity-0 transition-opacity duration-150", isCopied && "text-primary opacity-100")}>{isCopied ? "Copied" : "Copy"}</span>
      </Button>
      {children}
    </Item>
  );
}

export function LocalApiSheet({
  state,
  frozen,
  clientKey,
  clientKeyVisible,
  copied,
  externalError,
  onCopy,
  onToggleKey,
  onRotate,
  onSave,
  onClose,
}: {
  state: AppState;
  frozen: boolean;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  externalError?: string;
  onCopy(label: string, value: string): Promise<void>;
  onToggleKey(): void;
  onRotate(): Promise<string | undefined>;
  onSave(config: ListenConfig): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [draft, setDraft] = useState<ListenConfig>(state.localApi);
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const reportError = useErrorAlert("Local API action failed", externalError);
  const rotateKey = async () => {
    setSaving(true);
    try {
      const confirmed = await desktopApi.confirm({
        title: "Rotate local API key?",
        message: "The old client key will stop working immediately. Update your tools with the new key. Agent credentials do not change. In-flight requests may be interrupted.",
        confirmLabel: "Rotate key",
      });
      if (confirmed) {
        const message = await onRotate();
        if (message) reportError(message);
      }
    } catch (error) {
      reportError(error);
    } finally {
      setSaving(false);
    }
  };
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
        message: `Listen on ${draft.listenAddress}:${draft.port}? The local API uses unencrypted HTTP. Only use a trusted network, and never expose this port to the internet.`,
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
    <Sheet title="Local API settings" className="local-api-sheet w-[min(560px,_calc(var(--window-dialog-width,_100vw)_-_32px))] h-[min(512px,_calc(var(--window-dialog-height,_100vh)_-_32px))] [&_.sheet-card]:mt-3 form-sheet [&_>_.sheet-heading]:px-5 [&_>_.field-note]:mx-5 [&_.sheet-footer]:mx-5 [&_form_>_[data-slot=field-error]]:mx-5 [&_.sheet-scroll]:px-5" dismissible={!saving} onClose={onClose}>
      <form onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-4">
          <FieldGroup>
          <ListenerFields api={desktopApi} idPrefix="local" value={draft} minPort={1024} clientHostNote="Optional host for client URLs and agent configs. Does not change the listener." disabled={frozen || saving} onChange={setDraft} />
          <FieldSeparator />
          <Field>
            <FieldLabel htmlFor="local-client-key">Client key</FieldLabel>
            <InputGroup>
              <InputGroupInput id="local-client-key" className="mono font-mono text-xs" type={clientKeyVisible ? "text" : "password"} value={clientKey} readOnly />
              <InputGroupAddon align="inline-end">
                <Hint content={clientKeyVisible ? "Hide client key" : "Reveal client key"}><InputGroupButton size="icon-xs" aria-label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff /> : <Eye />}</InputGroupButton></Hint>
                <Hint content="Copy client key"><InputGroupButton size="icon-xs" aria-label="Copy client key" disabled={saving || !clientKey} onClick={() => void onCopy("Client key", clientKey)}>{copied === "Client key" ? <Check /> : <Copy />}</InputGroupButton></Hint>
                <Hint content="Rotate key"><InputGroupButton size="icon-xs" aria-label="Rotate key" disabled={frozen || saving} onClick={() => void rotateKey()}><RefreshCw /></InputGroupButton></Hint>
              </InputGroupAddon>
            </InputGroup>
            {copied === "Client key" && <FieldDescription role="status">Copied</FieldDescription>}
          </Field>
          </FieldGroup>
        </div>
        <SheetActions leading={
          <Button type="button" variant="outline" disabled={frozen || saving} onClick={() => setDraft({ listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 })}>Use default</Button>
        }>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>{frozen ? "Done" : "Cancel"}</Button>
          <Button type="submit" variant="default" disabled={frozen || saving}>{saving ? "Saving…" : "Save"}</Button>
        </SheetActions>
      </form>
    </Sheet>
  );
}
