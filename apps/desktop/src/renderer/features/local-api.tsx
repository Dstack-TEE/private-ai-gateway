import React, { useState } from "react";
import { errorMessage } from "../lib/error-message";
import { Check, Copy, Eye, EyeOff, RefreshCw } from "lucide-react";
import { Button } from "../components/ui/button";
import { ListenAddress, localAddressKind } from "../components/listen-address";
import { NetworkWarning } from "../components/network-warning";
import { Hint } from "../components/hint";
import { Field, FieldGroup, FieldLabel, FieldDescription, FieldError, FieldSeparator } from "../components/ui/field";
import { Item } from "../components/ui/item";
import { Input } from "../components/ui/input";
import { InputGroup, InputGroupInput, InputGroupAddon, InputGroupButton } from "../components/ui/input-group";
import { IconButton } from "../components/controls";
import { Sheet, SheetActions } from "../components/sheet";
import { FormField } from "../components/settings";
import type { GatewayState, LocalApiConfig } from "../../shared/contracts";
import { maskClientKey } from "../lib/format";
import { desktopApi } from "../lib/environment";

export function LocalApiPanel({
  proxyUrl,
  endpointError,
  clientKey,
  clientKeyVisible,
  copied,
  onCopy,
  onToggleKey,
}: {
  proxyUrl?: string;
  endpointError?: string;
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
      <Item variant="muted" size="xs" className="copy-row relative min-w-0 h-14 overflow-hidden">
        <Button variant="ghost"
          className="copy-surface absolute inset-0 min-w-0 min-h-0 pt-2.25 pr-[min(100px,_40%)] pb-2.25 pl-3 flex flex-col items-start justify-center gap-0.5 bg-transparent border-0 text-left [&_>_*]:max-w-full [&_>_.row-title-line]:w-full [&_>_.row-title-line]:min-w-0 [&_>_.row-note]:w-full [&_>_.row-note]:min-w-0 [&_>_.row-title-line]:overflow-hidden [&_>_.row-title-line_>_*]:min-w-0 [&_>_.row-title-line_>_*]:overflow-hidden [&_>_.row-title-line_>_*]:text-ellipsis [&_>_.row-title-line_>_*]:whitespace-nowrap [&_>_.row-note]:flex-none [&_.row-title]:text-muted-foreground [&_.row-title]:text-xs [&_.row-title]:font-normal [&_code.row-note]:text-foreground [&_code.row-note]:text-sm hover:bg-muted [&_code]:max-w-full [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [&:hover_.copy-feedback]:opacity-100 [&:focus-visible_.copy-feedback]:opacity-100 h-full w-full rounded-none"
          disabled={!proxyUrl}
          aria-label={`${endpointLabel}: ${proxyUrl ?? "Unavailable"}. Copy`}
          onClick={() => proxyUrl && void onCopy(endpointLabel, proxyUrl)}
        >
          <span className="row-title-line max-w-full flex items-center flex-wrap gap-y-1 gap-x-2">
            <span className="row-title">Endpoint</span>
          </span>
          <code className="row-note flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap">{proxyUrl ?? "Unavailable"}</code>
          <span className={`copy-feedback absolute right-13.5 top-[50%] opacity-0 -translate-y-1/2 text-muted-foreground text-xs font-semibold [transition:opacity_120ms_ease] [&.is-copied]:opacity-100 [&.is-copied]:text-primary ${copied === endpointLabel ? "is-copied" : ""}`}>{copied === endpointLabel ? "Copied" : "Copy"}</span>
        </Button>
      </Item>
      <Item variant="muted" size="xs" className="copy-row relative min-w-0 h-14 overflow-hidden">
        <Button variant="ghost" className="copy-surface absolute inset-0 min-w-0 min-h-0 pt-2.25 pr-[min(100px,_40%)] pb-2.25 pl-3 flex flex-col items-start justify-center gap-0.5 bg-transparent border-0 text-left [&_>_*]:max-w-full [&_>_.row-title-line]:w-full [&_>_.row-title-line]:min-w-0 [&_>_.row-note]:w-full [&_>_.row-note]:min-w-0 [&_>_.row-title-line]:overflow-hidden [&_>_.row-title-line_>_*]:min-w-0 [&_>_.row-title-line_>_*]:overflow-hidden [&_>_.row-title-line_>_*]:text-ellipsis [&_>_.row-title-line_>_*]:whitespace-nowrap [&_>_.row-note]:flex-none [&_.row-title]:text-muted-foreground [&_.row-title]:text-xs [&_.row-title]:font-normal [&_code.row-note]:text-foreground [&_code.row-note]:text-sm hover:bg-muted [&_code]:max-w-full [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [&:hover_.copy-feedback]:opacity-100 [&:focus-visible_.copy-feedback]:opacity-100 h-full w-full rounded-none" disabled={!clientKey} aria-label={`${keyLabel}: ${clientKeyVisible ? clientKey : "hidden"}. Copy`} onClick={() => clientKey && void onCopy(keyLabel, clientKey)}>
          <span className="row-title-line max-w-full flex items-center flex-wrap gap-y-1 gap-x-2">
            <span className="row-title">Client key</span>
          </span>
          <code className="row-note flex-[1_0_100%] block text-muted-foreground text-xs wrap-anywhere [&_code]:overflow-hidden [&_code]:text-ellipsis [&_code]:whitespace-nowrap [code&]:overflow-hidden [code&]:text-ellipsis [code&]:whitespace-nowrap">{clientKey ? clientKeyVisible ? clientKey : maskClientKey(clientKey) : "Unavailable"}</code>
          <span className={`copy-feedback absolute right-13.5 top-[50%] opacity-0 -translate-y-1/2 text-muted-foreground text-xs font-semibold [transition:opacity_120ms_ease] [&.is-copied]:opacity-100 [&.is-copied]:text-primary ${copied === keyLabel ? "is-copied" : ""}`}>{copied === keyLabel ? "Copied" : "Copy"}</span>
        </Button>
        <IconButton className="row-action relative z-2 ml-auto" label={clientKeyVisible ? "Hide client key" : "Reveal client key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}</IconButton>
      </Item>
      {endpointError && <p className="inline-error pt-2 pr-3.25 pb-2 pl-3.25 text-destructive bg-[var(--danger-bg)] text-xs">{endpointError}</p>}
    </div>
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
  state: GatewayState;
  frozen: boolean;
  clientKey: string;
  clientKeyVisible: boolean;
  copied?: string;
  externalError?: string;
  onCopy(label: string, value: string): Promise<void>;
  onToggleKey(): void;
  onRotate(): Promise<void>;
  onSave(config: LocalApiConfig): Promise<string | undefined>;
  onClose(): void;
}): React.JSX.Element {
  const [draft, setDraft] = useState<LocalApiConfig>(state.localApi);
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const update = <Key extends keyof LocalApiConfig>(key: Key, value: LocalApiConfig[Key]) => {
    setDraft((current) => ({ ...current, [key]: value }));
    setError(undefined);
  };
  const rotateKey = async () => {
    setSaving(true);
    setError(undefined);
    try {
      const confirmed = await desktopApi.confirm({
        title: "Rotate local API key?",
        message: "The old client key will stop working immediately. Update your tools with the new key. Agent credentials do not change. In-flight requests may be interrupted.",
        confirmLabel: "Rotate key",
      });
      if (confirmed) await onRotate();
    } catch (error) {
      setError(errorMessage(error));
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
      if (networkAccess && !await desktopApi.confirm({
        title: "Allow network access?",
        message: `Listen on ${draft.listenAddress}:${draft.port}? The local API uses unencrypted HTTP. Only use a trusted network, and never expose this port to the internet.`,
        confirmLabel: "Allow and Save",
      })) return;
      const message = await onSave({ ...draft, allowNetworkAccess: networkAccess });
      setError(message);
      if (!message) onClose();
    } catch (saveError) {
      setError(errorMessage(saveError));
    } finally {
      setSaving(false);
    }
  };
  return (
    <Sheet title="Local API settings" className="local-api-sheet w-[min(560px,_calc(var(--window-dialog-width,_100vw)_-_32px))] h-[min(512px,_calc(var(--window-dialog-height,_100vh)_-_32px))] [&_.sheet-card]:mt-3 form-sheet [&_>_.sheet-heading]:px-5 [&_>_.field-note]:mx-5 [&_.sheet-footer]:mx-5 [&_form_>_[data-slot=field-error]]:mx-5 [&_.sheet-scroll]:px-5" dismissible={!saving} onClose={onClose}>
      <form onSubmit={(event) => void submit(event)}>
        <div className="sheet-scroll py-4">
          <FieldGroup>
          <div className="grid grid-cols-[minmax(0,1fr)_7rem] items-start gap-4">
          <Field>
            <div className="flex min-h-5 items-center gap-2"><FieldLabel htmlFor="local-listen-address">Listen address</FieldLabel>{networkAccess && <NetworkWarning />}</div>
            <ListenAddress api={desktopApi} value={draft.listenAddress} disabled={frozen || saving} onChange={(value) => update("listenAddress", value)} />
          </Field>
          <Field>
            <FieldLabel className="min-h-5" htmlFor="local-port">Port</FieldLabel>
            <Input id="local-port" type="number" min="1024" max="65535" required value={draft.port} disabled={frozen || saving} onChange={(event) => update("port", Number(event.target.value))} />
          </Field>
          </div>
          <FormField id="local-client-host" label="Client host" description={addressKind === "unspecified" ? "Required for all-interface listeners. Use an address reachable by your clients." : "Optional host for client URLs and agent configs. Does not change the listener."}>
            <Input id="local-client-host" aria-describedby="local-client-host-note" value={draft.clientHost ?? ""} required={addressKind === "unspecified"} placeholder="Same as listen address" disabled={frozen || saving} spellCheck={false} autoComplete="off" onChange={(event) => update("clientHost", event.target.value || undefined)} />
          </FormField>
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
        <FieldError className="mt-3">{error ?? externalError}</FieldError>
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
