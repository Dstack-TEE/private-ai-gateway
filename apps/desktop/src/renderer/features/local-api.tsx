import React, { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Check, Copy, Eye, EyeOff, RefreshCw } from "lucide-react";
import { Button } from "../components/ui/button";
import { ListenerFields } from "../components/listen-address";
import { localAddressKind } from "../lib/local-api-config";
import { Hint } from "../components/hint";
import { Field, FieldGroup, FieldLabel, FieldError, FieldSeparator } from "../components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "../components/ui/item";
import { InputGroup, InputGroupInput, InputGroupAddon, InputGroupButton } from "../components/ui/input-group";
import { IconButton } from "../components/controls";
import { AppDialog, type DialogControl } from "../components/app-dialog";
import { useConfirm } from "../components/confirm";
import { DialogFooter } from "../components/ui/dialog";
import { DEFAULT_LOCAL_API_CONFIG, type AppState, type ListenConfig } from "../../shared/contracts";
import { maskClientKey } from "../lib/format";
import { desktopApi } from "../lib/environment";
import { errorMessage, toastError } from "../lib/error-message";
import { useCopy } from "../hooks/use-copy";

export function LocalApiPanel({
  proxyUrl,
  clientKey,
  clientKeyVisible,
  onToggleKey,
}: {
  proxyUrl?: string;
  clientKey: string;
  clientKeyVisible: boolean;
  onToggleKey(): void;
}): React.JSX.Element {
  const { copy, isCopied, status } = useCopy((error, label) => toastError(`Could not copy the ${label}`, error));
  return (
    <>
      <ItemGroup>
        <CopyRow title="Endpoint" copyLabel="Local API endpoint" value={proxyUrl} copied={isCopied(proxyUrl)} onCopy={copy} />
        <CopyRow
          title="API key"
          copyLabel="Local API key"
          value={clientKey || undefined}
          displayValue={clientKey && !clientKeyVisible ? maskClientKey(clientKey) : undefined}
          copied={isCopied(clientKey)}
          onCopy={copy}
        >
          <IconButton size="icon-sm" label={clientKeyVisible ? "Hide Local API key" : "Show Local API key"} disabled={!clientKey} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff /> : <Eye />}</IconButton>
        </CopyRow>
      </ItemGroup>
      {status}
    </>
  );
}

function CopyRow({
  title,
  copyLabel,
  value,
  displayValue = value,
  copied,
  onCopy,
  children,
}: React.PropsWithChildren<{
  title: string;
  copyLabel: string;
  value?: string;
  displayValue?: string;
  copied: boolean;
  onCopy(label: string, value: string): void;
}>): React.JSX.Element {
  return (
    <Item variant="muted" size="xs">
      <ItemContent className="min-w-0">
        <ItemTitle>{title}</ItemTitle>
        <ItemDescription><code className="break-all">{displayValue ?? "Unavailable"}</code></ItemDescription>
      </ItemContent>
      <ItemActions>
        {children}
        <IconButton size="icon-sm" label={`Copy ${copyLabel}`} disabled={!value} onClick={() => { if (value) onCopy(copyLabel, value); }}>{copied ? <Check /> : <Copy />}</IconButton>
      </ItemActions>
    </Item>
  );
}

export function LocalApiDialog({
  state,
  clientKey,
  clientKeyVisible,
  onToggleKey,
  onRotate,
  onSave,
  onClose,
  ...control
}: {
  state: AppState;
  clientKey: string;
  clientKeyVisible: boolean;
  onToggleKey(): void;
  onRotate(): Promise<void>;
  onSave(config: ListenConfig): Promise<void>;
} & DialogControl): React.JSX.Element {
  const frozen = state.status === "verifying";
  const [draft, setDraft] = useState<ListenConfig>(state.localApi);
  const addressKind = localAddressKind(draft.listenAddress);
  const networkAccess = Boolean(addressKind && addressKind !== "loopback");
  const [error, setError] = useState<string>();
  const report = (failure: unknown) => setError(errorMessage(failure));
  const confirm = useConfirm();
  const { copy, isCopied, status } = useCopy(report);
  const rotate = useMutation({
    mutationFn: async () => {
      if (await confirm({
        title: "Rotate the Local API key?",
        message: "The old key stops working immediately. Update your tools with the new key. Agent credentials do not change. In-flight requests may be interrupted.",
        confirmLabel: "Rotate Key",
        destructive: true,
      })) await onRotate();
    },
    onMutate: () => setError(undefined),
    onError: report,
  });
  // Resolves whether the settings were saved.
  const save = useMutation({
    mutationFn: async () => {
      if (!addressKind) throw new Error("Enter a valid IPv4 or IPv6 listen address.");
      if (networkAccess && !await confirm({
        title: "Allow network access?",
        message: `Listen on ${draft.listenAddress}:${draft.port}? The local API uses unencrypted HTTP. Only use a trusted network, and never expose this port to the internet.`,
        confirmLabel: "Allow and Save",
      })) return false;
      await onSave({ ...draft, allowNetworkAccess: networkAccess });
      return true;
    },
    onMutate: () => setError(undefined),
    onSuccess: (saved) => { if (saved) onClose(); },
    onError: report,
  });
  const saving = rotate.isPending || save.isPending;
  const copyKey = () => {
    setError(undefined);
    copy("Local API key", clientKey);
  };
  return (
    <AppDialog {...control} title="Local API settings" className="sm:max-w-xl" dismissible={!saving} onClose={onClose}>
      <form className="flex min-h-0 flex-col gap-4" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
        <div className="-mx-6 min-h-0 overflow-y-auto px-6 py-1">
          <FieldGroup>
          <ListenerFields api={desktopApi} idPrefix="local" value={draft} minPort={1024} clientHostNote="Optional host for client URLs and agent configs. Does not change the listener." disabled={frozen || saving} onChange={setDraft} />
          <FieldSeparator />
          <Field>
            <FieldLabel htmlFor="local-client-key">Local API key</FieldLabel>
            <InputGroup>
              <InputGroupInput id="local-client-key" className="font-mono" type={clientKeyVisible ? "text" : "password"} value={clientKey} readOnly />
              <InputGroupAddon align="inline-end">
                <Hint content={clientKeyVisible ? "Hide Local API key" : "Show Local API key"}><InputGroupButton size="icon-xs" aria-label={clientKeyVisible ? "Hide Local API key" : "Show Local API key"} onClick={onToggleKey}>{clientKeyVisible ? <EyeOff /> : <Eye />}</InputGroupButton></Hint>
                <Hint content="Copy Local API key"><InputGroupButton size="icon-xs" aria-label="Copy Local API key" disabled={saving || !clientKey} onClick={copyKey}>{isCopied(clientKey) ? <Check /> : <Copy />}</InputGroupButton></Hint>
                <Hint content="Rotate key"><InputGroupButton size="icon-xs" aria-label="Rotate key" disabled={frozen || saving} onClick={() => rotate.mutate()}><RefreshCw /></InputGroupButton></Hint>
              </InputGroupAddon>
            </InputGroup>
            {!clientKey && <FieldError>The Local API key is unavailable. Rotate it to restore access.</FieldError>}
          </Field>
          </FieldGroup>
        </div>
        <FieldError>{error}</FieldError>
        {status}
        <DialogFooter>
          <Button type="button" variant="outline" className="sm:mr-auto" disabled={frozen || saving} onClick={() => setDraft(DEFAULT_LOCAL_API_CONFIG)}>Use Default</Button>
          <Button type="button" variant="outline" onClick={onClose} disabled={saving}>{frozen ? "Done" : "Cancel"}</Button>
          <Button type="submit" variant="default" disabled={frozen || saving}>{saving ? "Saving…" : "Save"}</Button>
        </DialogFooter>
      </form>
    </AppDialog>
  );
}
